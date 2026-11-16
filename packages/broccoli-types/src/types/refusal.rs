//! The codes a plugin refuses a request with.

/// One of a plugin's refusal codes: a code its routes and hooks refuse a
/// request with. The host sends it as `<plugin id>.<CODE>`, and a client shows
/// the text `<plugin id>.apiError.<CODE>` from the plugin's pack in the
/// user's language. A plugin declares every code it refuses with in one enum
/// ([`refusal_codes!`](crate::refusal_codes)), and one test of the plugin
/// (`broccoli_server_sdk::testing::assert_refusal_codes_translated`) pins
/// that each has a text in every pack, so no refusal reaches a user as a
/// generic message that says something else.
pub trait RefusalCode: Copy + 'static {
    /// Every code of the enum.
    const ALL: &'static [Self];

    /// The code as it goes out, in `SCREAMING_SNAKE_CASE`.
    fn as_str(self) -> &'static str;
}

/// Declare a plugin's refusal codes as an enum implementing [`RefusalCode`]:
///
/// ```
/// broccoli_types::refusal_codes! {
///     pub enum Refusal {
///         /// The job is gone.
///         JobNotFound = "JOB_NOT_FOUND",
///     }
/// }
/// ```
#[macro_export]
macro_rules! refusal_codes {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident = $code:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $crate::types::RefusalCode for $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $code),+
                }
            }
        }
    };
}

crate::refusal_codes! {
    /// The refusals the SDK itself answers a plugin's route with, which
    /// every plugin with routes translates like its own.
    pub enum SdkRefusal {
        /// A path parameter is missing or does not parse.
        InvalidPathParameter = "INVALID_PATH_PARAMETER",
        /// The route needs a signed-in caller.
        SignInRequired = "SIGN_IN_REQUIRED",
    }
}
