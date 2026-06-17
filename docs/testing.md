# Testing guidelines

## Rendering themes

Rendering and theme behavior tests should use artificial fixture themes with
explicit semantic attributes. Do not snapshot or assert details of Tau's built-in
themes from renderer tests; built-ins are product defaults and may change for
readability without implying renderer behavior changed.

Built-in theme tests should be limited to parsing and intentional invariants of
those built-ins, such as the conservative default theme staying within its
allowed safe foreground colors and avoiding background colors.
