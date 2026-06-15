# tau-cli design decisions

This file records local terminal-UI design decisions that future changes should
preserve unless the project intentionally revisits them. It complements the
crate README/AGENTS instructions with durable rationale for transcript rendering
and other UI boundaries.

## Markdown-lite transcript styling

Status: confirmed, 2026-06-15, dpc


Tau applies Markdown-lite formatting in the terminal UI only. The harness,
protocol events, durable agent logs, prompt previews, model context, and other
clients continue to see the original plain text.

The formatter is deliberately small. It recognizes headings, unordered and
ordered list markers, `*strong*`, `_emphasis_`, basic backslash escapes, and
leading-pipe tables. Most constructs are style-only and preserve exact source
characters rather than stripping delimiters or rewriting list/header prefixes.
Tables are the exception: the UI may add bounded display-only padding spaces so
cells align while the visible text remains valid Markdown table syntax. Inline
backticks, fenced code blocks, and indented code-like lines get code styling and
suppress nested Markdown-lite styling; escaped marker sequences get escape
styling. This keeps live terminal wrapping, scrollback, and copy/paste behavior
stable outside intentional table padding.

Live response and thinking blocks use an append-aware cache. Text before a blank
line is treated as sealed and parsed once; the current unsealed suffix remains
base-styled until a future update seals it. The cache also preserves parser
context, including open fenced code blocks, across sealed chunks. Final/static
blocks parse the complete string immediately.

Formatting is scoped to submitted user prompts, assistant response text, and
reasoning/thinking text. Tool calls, tool payloads/results, shell output,
status/progress lines, and agent-to-agent message debug displays must stay on
their existing renderers unless there is a separate product decision.
