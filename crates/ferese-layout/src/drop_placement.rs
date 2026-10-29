/// The destination of an interactive tiled-window drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropPlacement {
    Left,
    Right,
    Above,
    Below,
    Tab,
}
