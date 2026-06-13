# tau-ext-telegram security notes

- The built-in extension is disabled by default and requires an explicit bot
  token secret and non-empty `allowed_user_ids` allowlist.
- Telegram bot tokens are secrets. Do not derive `Debug` for structs containing
  token text and do not include Bot API URLs in error strings.
- The model cannot choose arbitrary chat ids. `telegram_send` uses only the
  configured `chat_id` or an allowlisted user's linked `/start` private chat.
- Messages from users outside `allowed_user_ids` are ignored before any routing
  or Telegram reply side effects.
- Unconfigured group/supergroup chats are refused. Groups are accepted only when
  their `chat_id` is explicitly configured by the user.
- Text is treated as untrusted user input and is prefixed with Telegram source
  context before being submitted to Tau.
