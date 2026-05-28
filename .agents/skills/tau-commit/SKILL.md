---
name: tau-commit
description: >
  Use before writing Tau commit descriptions.
user-invocable: true
advertise: true
---

# Tau commit descriptions

* Use conventional commit title format: `type(scope): summary`.
* Add `### Summary`: one short paragraph explaining what and why.
* Add `### Details`: relevant implementation, verification, and caveats.
* Add `### Summary of the original prompt` when the user request is available.
* Keep all sections concise; do not leave placeholder sections.
