#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferPosition {
    pub line: usize,
    pub column: usize,
}

pub type LineSelection = Option<(usize, usize)>;

#[derive(Default, Debug, Clone)]
pub enum Selection {
    #[default]
    None,
    Selecting {
        origin: BufferPosition,
        from: BufferPosition,
        to: BufferPosition,
    },
    Selected {
        from: BufferPosition,
        to: BufferPosition,
    },
}

impl Selection {
    pub fn for_line(&self, line_number: usize) -> LineSelection {
        match self {
            Selection::None => None,
            Selection::Selecting {
                from,
                to,
                origin: _,
            }
            | Selection::Selected { from, to } => {
                // see if this line_number fals in the range of from.line_number..=to.line_number
                if from.line <= line_number && to.line >= line_number {
                    Some((
                        if from.line == line_number {
                            from.column
                        } else {
                            0
                        },
                        if to.line == line_number {
                            to.column
                        } else {
                            usize::MAX
                        },
                    ))
                } else {
                    None
                }
            }
        }
    }
}
