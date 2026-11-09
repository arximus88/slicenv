# Linux window frame

The app uses GPUI client-side decorations (`WindowDecorations::Client`) so the
workspace draws its own one-pixel frame and keeps resize hit zones around the
window. `gpui_component::window_border()` handles those hit zones and respects
the compositor's tiling edges, allowing the frame to disappear on tiled sides.

In gpui-component 0.8.1, the default Linux client inset is 20 px. That inset is
intended to provide room for the frame's shadow, but GPUI windows default to an
opaque native background. The transparent outer gutter therefore appeared as
a solid system-colored matte. The app now sets
`WindowBackgroundAppearance::Transparent` and uses a 6 px inset. The workspace
surface remains painted by the app; only the outer gutter reveals the
compositor background and shadow.

The 6 px inset is also supplied to GPUI as the client inset. The component
continues to use its existing resize hit zones and calls GPUI's native resize
operation, preserving compositor-managed resizing on X11 and Wayland. At a
tiled edge it removes visual padding and the border for that edge, while
retaining a stable client inset for restore/resize behavior.

Headless builds and Xvfb checks can verify compilation and rendering paths but
cannot establish how a real desktop compositor presents transparency, shadows,
resize gestures, or tiling. Those details still need visual confirmation on a
Linux desktop session.
