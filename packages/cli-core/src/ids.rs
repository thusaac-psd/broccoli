//! The ids a request path names. What a user types (a contest's title, a
//! problem's label or position) becomes one only through [`crate::resolve`],
//! so no unresolved text reaches a request path.

use std::fmt;

/// A contest's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContestId(i32);

/// A problem's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProblemId(i32);

macro_rules! id {
    ($name:ident) => {
        impl $name {
            /// The id as the server sent it in an answer (a contest list, a
            /// submission). An id a user typed comes from [`crate::resolve`].
            pub fn from_server(id: i32) -> Self {
                Self(id)
            }

            pub fn get(self) -> i32 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id!(ContestId);
id!(ProblemId);
