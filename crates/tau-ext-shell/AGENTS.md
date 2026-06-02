# ext-shell

File-mutation tools such as `edit` and `apply_patch` MUST attach structured UI-only diff payloads for changed UTF-8 files. The agent-visible tool result must stay minimal and must not include the diff.

Diff payloads MUST preserve unified-diff rendering data, including intra-line changed word/phrase segments for paired single-line replacements, so UIs can apply separate inline theme styles on top of added/removed line styles.
