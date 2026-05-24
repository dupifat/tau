use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_imap::imap_proto::types::{Address as ImapAddress, BodyStructure, NameAttribute};
use async_imap::types::Flag;
use async_imap::{Client, Session};
use futures_util::TryStreamExt;
use lettre::message::header::ContentType as LettreContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{CertificateStore, Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use mail_parser::{Address as ParsedAddress, MessageParser, MimeHeaders};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::runtime::Runtime;
use tokio::time;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::{
    AuthMethod, BackendAttachment, BackendFolder, BackendMessage, BackendMessagePage, EmailBackend,
    OutgoingMessage, TlsMode, ValidatedAuthConfig, ValidatedConfig, ValidatedImapConfig,
    ValidatedSmtpConfig,
};

const AUTH_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const AUTH_COMMAND_MAX_STDOUT: usize = 64 * 1024;

/// Production IMAP/SMTP backend for configured email accounts.
pub struct RealEmailBackend {
    accounts: BTreeMap<String, RealAccount>,
    runtime: Runtime,
}

#[derive(Clone)]
struct RealAccount {
    imap: Option<ValidatedImapConfig>,
    smtp: Option<ValidatedSmtpConfig>,
    auth: Option<ValidatedAuthConfig>,
}

impl RealEmailBackend {
    /// Build a production backend from validated extension configuration.
    pub fn new(config: &ValidatedConfig) -> Result<Self, String> {
        let runtime = Runtime::new()
            .map_err(|error| format!("internal_error: failed to start email runtime: {error}"))?;
        let accounts = config
            .accounts
            .iter()
            .map(|(id, account)| {
                (
                    id.clone(),
                    RealAccount {
                        imap: account.imap.clone(),
                        smtp: account.smtp.clone(),
                        auth: account.auth.clone(),
                    },
                )
            })
            .collect();
        Ok(Self { accounts, runtime })
    }

    fn account(&self, id: &str) -> Result<RealAccount, String> {
        self.accounts
            .get(id)
            .cloned()
            .ok_or_else(|| "internal_error: account not found in backend".to_owned())
    }

    fn block_with_timeout<T, Fut>(&self, seconds: u64, fut: Fut) -> Result<T, String>
    where
        Fut: Future<Output = Result<T, String>>,
    {
        self.runtime.block_on(async move {
            match time::timeout(Duration::from_secs(seconds), fut).await {
                Ok(result) => result,
                Err(_) => Err("network_error: email backend operation timed out".to_owned()),
            }
        })
    }
}

impl EmailBackend for RealEmailBackend {
    fn list_folders(&self, account: &str) -> Result<Vec<BackendFolder>, String> {
        let account = self.account(account)?;
        let timeout_seconds = account.imap_config()?.timeout_seconds;
        self.block_with_timeout(timeout_seconds, async move {
            let mut session = connect_imap(&account).await?;
            let mut names = session.list(None, Some("*")).await.map_err(imap_error)?;
            let mut folders = Vec::new();
            while let Some(name) = names.try_next().await.map_err(imap_error)? {
                let selectable = !name.attributes().contains(&NameAttribute::NoSelect);
                folders.push(BackendFolder {
                    name: name.name().to_owned(),
                    delimiter: name.delimiter().unwrap_or("/").to_owned(),
                    selectable,
                });
            }
            drop(names);
            let _ = session.logout().await;
            Ok(folders)
        })
    }

    fn list_messages(&self, account: &str, folder: &str) -> Result<Vec<BackendMessage>, String> {
        self.list_messages_page(account, folder, 100, 0)
            .map(|page| page.messages)
    }

    fn list_messages_page(
        &self,
        account: &str,
        folder: &str,
        limit: usize,
        offset: usize,
    ) -> Result<BackendMessagePage, String> {
        let account = self.account(account)?;
        let timeout_seconds = account.imap_config()?.timeout_seconds;
        let folder = folder.to_owned();
        self.block_with_timeout(timeout_seconds, async move {
            list_messages_page_async(&account, &folder, limit, offset).await
        })
    }

    fn message_metadata(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
    ) -> Result<BackendMessage, String> {
        let account = self.account(account)?;
        let timeout_seconds = account.imap_config()?.timeout_seconds;
        let folder = folder.to_owned();
        let uid = uid.to_owned();
        self.block_with_timeout(timeout_seconds, async move {
            message_metadata_async(&account, &folder, &uid).await
        })
    }

    fn read_message(
        &self,
        account: &str,
        folder: &str,
        uid: &str,
    ) -> Result<BackendMessage, String> {
        let account = self.account(account)?;
        let timeout_seconds = account.imap_config()?.timeout_seconds;
        let folder = folder.to_owned();
        let uid = uid.to_owned();
        self.block_with_timeout(timeout_seconds, async move {
            read_message_async(&account, &folder, &uid).await
        })
    }

    fn send_message(&mut self, message: &OutgoingMessage) -> Result<String, String> {
        let account = self.account(&message.account)?;
        let timeout_seconds = account.smtp_config()?.timeout_seconds;
        let message = clone_outgoing_message(message);
        self.block_with_timeout(timeout_seconds, async move {
            send_message_async(&account, &message).await
        })
    }
}

impl RealAccount {
    fn imap_config(&self) -> Result<&ValidatedImapConfig, String> {
        self.imap
            .as_ref()
            .ok_or_else(|| "imap_error: account has no IMAP configuration".to_owned())
    }

    fn smtp_config(&self) -> Result<&ValidatedSmtpConfig, String> {
        self.smtp
            .as_ref()
            .ok_or_else(|| "smtp_error: account has no SMTP configuration".to_owned())
    }
}

#[derive(Debug)]
enum RealImapStream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for RealImapStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for RealImapStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
        }
    }
}

async fn list_messages_page_async(
    account: &RealAccount,
    folder: &str,
    limit: usize,
    offset: usize,
) -> Result<BackendMessagePage, String> {
    let mut session = connect_imap(account).await?;
    let mailbox = session.examine(folder).await.map_err(imap_error)?;
    let uidvalidity = mailbox
        .uid_validity
        .map(|value| value.to_string())
        .unwrap_or_default();
    let mut uids = session
        .uid_search("ALL")
        .await
        .map_err(imap_error)?
        .into_iter()
        .collect::<Vec<_>>();
    uids.sort_unstable_by(|left, right| right.cmp(left));
    let truncated = uids.len() > offset.saturating_add(limit);
    let selected = uids
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        let _ = session.logout().await;
        return Ok(BackendMessagePage {
            messages: Vec::new(),
            next_cursor: None,
            truncated: false,
        });
    }

    let uid_set = selected
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut fetches = session
        .uid_fetch(uid_set, "(UID FLAGS INTERNALDATE ENVELOPE BODYSTRUCTURE)")
        .await
        .map_err(imap_error)?;
    let mut by_uid = BTreeMap::new();
    while let Some(fetch) = fetches.try_next().await.map_err(imap_error)? {
        let message = metadata_from_fetch(&fetch, &uidvalidity);
        by_uid.insert(message.uid.clone(), message);
    }
    drop(fetches);
    let _ = session.logout().await;

    let messages = selected
        .into_iter()
        .filter_map(|uid| by_uid.remove(&uid.to_string()))
        .collect();
    Ok(BackendMessagePage {
        messages,
        next_cursor: truncated.then(|| offset.saturating_add(limit).to_string()),
        truncated,
    })
}

async fn message_metadata_async(
    account: &RealAccount,
    folder: &str,
    uid: &str,
) -> Result<BackendMessage, String> {
    let mut session = connect_imap(account).await?;
    let mailbox = session.examine(folder).await.map_err(imap_error)?;
    let uidvalidity = mailbox
        .uid_validity
        .map(|value| value.to_string())
        .unwrap_or_default();
    let mut fetches = session
        .uid_fetch(uid, "(UID FLAGS INTERNALDATE ENVELOPE BODYSTRUCTURE)")
        .await
        .map_err(imap_error)?;
    let message = match fetches.try_next().await.map_err(imap_error)? {
        Some(fetch) => metadata_from_fetch(&fetch, &uidvalidity),
        None => return Err("message_not_found: message not found".to_owned()),
    };
    drop(fetches);
    let _ = session.logout().await;
    Ok(message)
}

async fn read_message_async(
    account: &RealAccount,
    folder: &str,
    uid: &str,
) -> Result<BackendMessage, String> {
    let mut session = connect_imap(account).await?;
    let mailbox = session.examine(folder).await.map_err(imap_error)?;
    let uidvalidity = mailbox
        .uid_validity
        .map(|value| value.to_string())
        .unwrap_or_default();
    let mut fetches = session
        .uid_fetch(
            uid,
            "(UID FLAGS INTERNALDATE ENVELOPE BODYSTRUCTURE BODY.PEEK[])",
        )
        .await
        .map_err(imap_error)?;
    let message = match fetches.try_next().await.map_err(imap_error)? {
        Some(fetch) => {
            let metadata = metadata_from_fetch(&fetch, &uidvalidity);
            let body = fetch
                .body()
                .ok_or_else(|| "message_not_found: message body not found".to_owned())?;
            parse_backend_message_from_rfc822(&metadata, body)
        }
        None => return Err("message_not_found: message not found".to_owned()),
    };
    drop(fetches);
    let _ = session.logout().await;
    Ok(message)
}

async fn send_message_async(
    account: &RealAccount,
    outgoing: &OutgoingMessage,
) -> Result<String, String> {
    let smtp = account.smtp_config()?;
    let message_id = generate_message_id(&smtp.host, outgoing);
    let email = build_lettre_message(outgoing, &message_id)?;
    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host)
        .port(smtp.port)
        .timeout(Some(Duration::from_secs(smtp.timeout_seconds)))
        .tls(smtp_tls(&smtp.host, smtp.tls)?);
    if let Some(password) = resolve_password(account.auth.as_ref()).await? {
        builder = builder.credentials(Credentials::new(smtp.login.clone(), password));
    }
    let mailer = builder.build();
    mailer
        .send(email)
        .await
        .map_err(|_| "smtp_error: SMTP send failed".to_owned())?;
    Ok(message_id)
}

async fn connect_imap(account: &RealAccount) -> Result<Session<RealImapStream>, String> {
    let imap = account.imap_config()?;
    let tcp = TcpStream::connect((imap.host.as_str(), imap.port))
        .await
        .map_err(|_| "network_error: IMAP connection failed".to_owned())?;
    let stream = match imap.tls {
        TlsMode::Required => RealImapStream::Tls(Box::new(tls_connect(&imap.host, tcp).await?)),
        TlsMode::StartTls | TlsMode::None => RealImapStream::Plain(tcp),
    };
    let mut client = Client::new(stream);
    read_imap_greeting(&mut client).await?;
    if imap.tls == TlsMode::StartTls {
        client
            .run_command_and_check_ok("STARTTLS", None)
            .await
            .map_err(imap_error)?;
        let tcp = match client.into_inner() {
            RealImapStream::Plain(tcp) => tcp,
            RealImapStream::Tls(_) => {
                return Err("tls_error: IMAP STARTTLS stream state was invalid".to_owned());
            }
        };
        client = Client::new(RealImapStream::Tls(Box::new(
            tls_connect(&imap.host, tcp).await?,
        )));
    }
    let password = resolve_password(account.auth.as_ref())
        .await?
        .ok_or_else(|| "auth_error: IMAP password source is not configured".to_owned())?;
    client
        .login(&imap.login, password)
        .await
        .map_err(|_| "auth_error: IMAP authentication failed".to_owned())
}

async fn read_imap_greeting(client: &mut Client<RealImapStream>) -> Result<(), String> {
    client
        .read_response()
        .await
        .map_err(|_| "network_error: IMAP greeting failed".to_owned())?
        .ok_or_else(|| "network_error: IMAP server closed before greeting".to_owned())?;
    Ok(())
}

async fn tls_connect(host: &str, tcp: TcpStream) -> Result<TlsStream<TcpStream>, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = rustls::crypto::ring::default_provider();
    let config = ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|_| "tls_error: failed to configure TLS versions".to_owned())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| "tls_error: invalid TLS server name".to_owned())?;
    TlsConnector::from(Arc::new(config))
        .connect(server_name, tcp)
        .await
        .map_err(|_| "tls_error: TLS handshake failed".to_owned())
}

fn smtp_tls(host: &str, mode: TlsMode) -> Result<Tls, String> {
    let params = || {
        TlsParameters::builder(host.to_owned())
            .certificate_store(CertificateStore::WebpkiRoots)
            .build()
            .map_err(|_| "tls_error: failed to configure SMTP TLS".to_owned())
    };
    match mode {
        TlsMode::Required => Ok(Tls::Wrapper(params()?)),
        TlsMode::StartTls => Ok(Tls::Required(params()?)),
        TlsMode::None => Ok(Tls::None),
    }
}

async fn resolve_password(auth: Option<&ValidatedAuthConfig>) -> Result<Option<String>, String> {
    let Some(auth) = auth else {
        return Ok(None);
    };
    match auth.method {
        AuthMethod::None => Ok(None),
        AuthMethod::Oauth2 => Err("auth_error: OAuth authentication is not implemented".to_owned()),
        AuthMethod::Password | AuthMethod::Command => {
            if matches!(auth.method, AuthMethod::Password)
                && let Some(env) = &auth.password_env
            {
                match std::env::var(env) {
                    Ok(password) if !password.is_empty() => return Ok(Some(password)),
                    Ok(_) => {
                        return Err("auth_error: password environment variable is empty".to_owned());
                    }
                    Err(_) if auth.command.is_some() => {}
                    Err(_) => {
                        return Err(
                            "auth_error: password environment variable is not set".to_owned()
                        );
                    }
                }
            }
            let Some(command) = &auth.command else {
                return Err("auth_error: password command is not configured".to_owned());
            };
            run_password_command(command, AUTH_COMMAND_TIMEOUT)
                .await
                .map(Some)
        }
    }
}

pub(crate) async fn run_password_command(
    command: &[String],
    timeout: Duration,
) -> Result<String, String> {
    let Some((program, args)) = command.split_first() else {
        return Err("auth_error: password command is empty".to_owned());
    };
    let stdout = time::timeout(timeout, async {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "auth_error: password command failed".to_owned())?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "auth_error: password command failed".to_owned())?;
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = stdout
                .read(&mut chunk)
                .await
                .map_err(|_| "auth_error: password command failed".to_owned())?;
            if read == 0 {
                break;
            }
            if buffer.len().saturating_add(read) > AUTH_COMMAND_MAX_STDOUT {
                return Err("auth_error: password command output is too large".to_owned());
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "auth_error: password command failed".to_owned())?;
        if !status.success() {
            return Err("auth_error: password command failed".to_owned());
        }
        Ok(buffer)
    })
    .await
    .map_err(|_| "auth_error: password command timed out".to_owned())??;
    let mut password = String::from_utf8(stdout)
        .map_err(|_| "auth_error: password command returned invalid UTF-8".to_owned())?;
    while password.ends_with(['\n', '\r']) {
        password.pop();
    }
    if password.is_empty() {
        return Err("auth_error: password command returned an empty password".to_owned());
    }
    Ok(password)
}

fn metadata_from_fetch(fetch: &async_imap::types::Fetch, uidvalidity: &str) -> BackendMessage {
    let envelope = fetch.envelope();
    let uid = fetch.uid.unwrap_or(fetch.message).to_string();
    let date = envelope
        .and_then(|env| env.date.as_ref())
        .map(|bytes| bytes_to_string(bytes.as_ref()))
        .or_else(|| fetch.internal_date().map(|date| date.to_rfc3339()))
        .unwrap_or_default();
    let from = envelope
        .and_then(|env| env.from.as_deref())
        .and_then(first_imap_address)
        .unwrap_or_default();
    let to = envelope
        .and_then(|env| env.to.as_deref())
        .map(imap_address_list)
        .unwrap_or_default();
    let cc = envelope
        .and_then(|env| env.cc.as_deref())
        .map(imap_address_list)
        .unwrap_or_default();
    let subject = envelope
        .and_then(|env| env.subject.as_ref())
        .map(|bytes| bytes_to_string(bytes.as_ref()))
        .unwrap_or_default();
    let message_id = envelope
        .and_then(|env| env.message_id.as_ref())
        .map(|bytes| bytes_to_string(bytes.as_ref()));
    let has_attachments = fetch
        .bodystructure()
        .is_some_and(bodystructure_has_attachment);
    BackendMessage {
        uid,
        uidvalidity: uidvalidity.to_owned(),
        date,
        from,
        to,
        cc,
        subject,
        body_text: String::new(),
        flags: fetch.flags().map(flag_to_string).collect(),
        has_attachments,
        attachments: Vec::new(),
        message_id,
    }
}

pub(crate) fn parse_backend_message_from_rfc822(
    fallback: &BackendMessage,
    raw: &[u8],
) -> BackendMessage {
    let Some(parsed) = MessageParser::default().parse(raw) else {
        let mut message = fallback.clone();
        message.body_text = "[message body omitted: RFC822 parse failed]".to_owned();
        message.attachments.clear();
        return message;
    };
    let mut message = fallback.clone();
    if let Some(from) = parsed.from().and_then(parsed_address_first) {
        message.from = from;
    }
    let to = parsed_address_list(parsed.to());
    if !to.is_empty() {
        message.to = to;
    }
    let cc = parsed_address_list(parsed.cc());
    if !cc.is_empty() {
        message.cc = cc;
    }
    if let Some(subject) = parsed.subject() {
        message.subject = subject.to_owned();
    }
    if let Some(date) = parsed.date() {
        message.date = date.to_string();
    }
    if let Some(message_id) = parsed.message_id() {
        message.message_id = Some(message_id.to_owned());
    }
    message.body_text = parsed_body_text(&parsed);
    message.attachments = parsed
        .attachments()
        .map(|part| BackendAttachment {
            filename: part.attachment_name().map(str::to_owned),
            content_type: part.content_type().map(content_type_string),
            size_bytes: Some(part.len() as u64),
        })
        .collect();
    message.has_attachments = message.has_attachments || !message.attachments.is_empty();
    message
}

fn parsed_body_text(parsed: &mail_parser::Message<'_>) -> String {
    let mut parts = Vec::new();
    for index in 0..parsed.text_body_count() {
        if let Some(text) = parsed.body_text(index) {
            parts.push(text.into_owned());
        }
    }
    if parts.is_empty()
        && let Some(html) = parsed.body_html(0)
    {
        parts.push(html.into_owned());
    }
    parts.join("\n")
}

fn build_lettre_message(outgoing: &OutgoingMessage, message_id: &str) -> Result<Message, String> {
    let mut builder = Message::builder()
        .from(
            outgoing
                .from
                .parse()
                .map_err(|_| "invalid_input: invalid From address".to_owned())?,
        )
        .subject(outgoing.subject.clone())
        .message_id(Some(message_id.to_owned()))
        .header(LettreContentType::TEXT_PLAIN);
    for recipient in &outgoing.to {
        builder = builder.to(recipient
            .parse()
            .map_err(|_| "invalid_input: invalid To address".to_owned())?);
    }
    for recipient in &outgoing.cc {
        builder = builder.cc(recipient
            .parse()
            .map_err(|_| "invalid_input: invalid Cc address".to_owned())?);
    }
    for recipient in &outgoing.bcc {
        builder = builder.bcc(
            recipient
                .parse()
                .map_err(|_| "invalid_input: invalid Bcc address".to_owned())?,
        );
    }
    if let Some(reply_to) = &outgoing.reply_to {
        builder = builder.reply_to(
            reply_to
                .parse()
                .map_err(|_| "invalid_input: invalid Reply-To address".to_owned())?,
        );
    }
    if let Some(in_reply_to) = &outgoing.in_reply_to {
        builder = builder.in_reply_to(in_reply_to.clone());
    }
    builder
        .body(outgoing.body_text.clone())
        .map_err(|_| "smtp_error: failed to build email message".to_owned())
}

fn generate_message_id(host: &str, outgoing: &OutgoingMessage) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let fingerprint = super::stable_id("smtp", outgoing);
    let domain = sanitized_message_id_domain(host);
    format!("<tau-{nanos}-{fingerprint}@{domain}>")
}

fn sanitized_message_id_domain(host: &str) -> String {
    let domain = host
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
        .collect::<String>();
    if domain.is_empty() {
        "tau.local".to_owned()
    } else {
        domain
    }
}

fn clone_outgoing_message(message: &OutgoingMessage) -> OutgoingMessage {
    OutgoingMessage {
        account: message.account.clone(),
        from: message.from.clone(),
        to: message.to.clone(),
        cc: message.cc.clone(),
        bcc: message.bcc.clone(),
        subject: message.subject.clone(),
        body_text: message.body_text.clone(),
        reply_to: message.reply_to.clone(),
        in_reply_to: message.in_reply_to.clone(),
    }
}

fn imap_error(error: async_imap::error::Error) -> String {
    match error {
        async_imap::error::Error::No(_) => {
            "imap_error: IMAP server rejected the command".to_owned()
        }
        async_imap::error::Error::Bad(_) => {
            "imap_error: IMAP server rejected the command".to_owned()
        }
        async_imap::error::Error::ConnectionLost => {
            "network_error: IMAP connection lost".to_owned()
        }
        async_imap::error::Error::Validate(_) => {
            "invalid_input: invalid IMAP command input".to_owned()
        }
        _ => "network_error: IMAP operation failed".to_owned(),
    }
}

fn bytes_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn first_imap_address(addresses: &[ImapAddress<'_>]) -> Option<String> {
    addresses.iter().find_map(imap_address_string)
}

fn imap_address_list(addresses: &[ImapAddress<'_>]) -> Vec<String> {
    addresses.iter().filter_map(imap_address_string).collect()
}

fn imap_address_string(address: &ImapAddress<'_>) -> Option<String> {
    let mailbox = String::from_utf8_lossy(address.mailbox.as_ref()?.as_ref());
    let host = String::from_utf8_lossy(address.host.as_ref()?.as_ref());
    Some(format!("{mailbox}@{host}"))
}

fn parsed_address_first(address: &ParsedAddress<'_>) -> Option<String> {
    parsed_address_list(Some(address)).into_iter().next()
}

fn parsed_address_list(address: Option<&ParsedAddress<'_>>) -> Vec<String> {
    match address {
        Some(ParsedAddress::List(addresses)) => addresses
            .iter()
            .filter_map(|address| address.address.as_deref().map(str::to_owned))
            .collect(),
        Some(ParsedAddress::Group(groups)) => groups
            .iter()
            .flat_map(|group| group.addresses.iter())
            .filter_map(|address| address.address.as_deref().map(str::to_owned))
            .collect(),
        None => Vec::new(),
    }
}

fn content_type_string(content_type: &mail_parser::ContentType<'_>) -> String {
    match content_type.subtype() {
        Some(subtype) => format!("{}/{}", content_type.ctype(), subtype),
        None => content_type.ctype().to_owned(),
    }
}

fn flag_to_string(flag: Flag<'_>) -> String {
    match flag {
        Flag::Seen => "seen".to_owned(),
        Flag::Answered => "answered".to_owned(),
        Flag::Flagged => "flagged".to_owned(),
        Flag::Deleted => "deleted".to_owned(),
        Flag::Draft => "draft".to_owned(),
        Flag::Recent => "recent".to_owned(),
        Flag::MayCreate => "may_create".to_owned(),
        Flag::Custom(value) => value.trim_start_matches('\\').to_ascii_lowercase(),
    }
}

fn bodystructure_has_attachment(body: &BodyStructure<'_>) -> bool {
    match body {
        BodyStructure::Basic { common, .. }
        | BodyStructure::Text { common, .. }
        | BodyStructure::Message { common, .. } => common_has_attachment(common),
        BodyStructure::Multipart { common, bodies, .. } => {
            common_has_attachment(common) || bodies.iter().any(bodystructure_has_attachment)
        }
    }
}

fn common_has_attachment(common: &async_imap::imap_proto::types::BodyContentCommon<'_>) -> bool {
    common
        .disposition
        .as_ref()
        .is_some_and(|disposition| disposition.ty.eq_ignore_ascii_case("attachment"))
        || body_params_have_name(
            common
                .disposition
                .as_ref()
                .and_then(|d| d.params.as_deref()),
        )
        || body_params_have_name(common.ty.params.as_deref())
}

fn body_params_have_name(
    params: Option<&[(std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)]>,
) -> bool {
    params.is_some_and(|params| {
        params.iter().any(|(name, _)| {
            name.eq_ignore_ascii_case("filename") || name.eq_ignore_ascii_case("name")
        })
    })
}

impl fmt::Debug for RealEmailBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealEmailBackend")
            .field("accounts", &self.accounts.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}
