//! Which family of 1.12 server the client speaks to. A stock vmangos/cmangos realm is
//! [`ServerFlavor::Vanilla`]; Turtle WoW and the cores derived from it are
//! [`ServerFlavor::Turtle`], whose wire differs from 1.12.1.5875 in three places this type owns.

/// Turtle WoW's final client build, 1.18.1.7272: Turtle-derived realmd answers any lower build
/// with `WOW_FAIL_VERSION_INVALID` at the proof stage.
pub const TWOW_CLIENT_BUILD: u16 = 7272;

/// The server family a login targets. Vanilla is the reference's own wire, byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerFlavor {
    /// A stock 1.12.1 server: build 5875 everywhere, Warden refused, a 1.12 char-create body.
    #[default]
    Vanilla,
    /// Turtle WoW 1.18.1 and its derivatives.
    Turtle,
}

impl ServerFlavor {
    /// The build `CMD_AUTH_LOGON_CHALLENGE` carries. The world hop keeps
    /// [`crate::CLIENT_BUILD`] on every flavor: Turtle's mangosd accepts exactly 5875 there.
    pub fn realmd_build(self) -> u16 {
        match self {
            ServerFlavor::Vanilla => crate::CLIENT_BUILD,
            ServerFlavor::Turtle => TWOW_CLIENT_BUILD,
        }
    }

    /// Whether `SMSG_WARDEN_DATA` during the handshake is skipped rather than refused. Turtle
    /// sends it on every login and does not kick an unanswered client; a stock vmangos with
    /// Warden on kicks after 30 s, so there the connect refuses ([`crate::WardenRequired`]).
    pub fn tolerates_warden(self) -> bool {
        self == ServerFlavor::Turtle
    }

    /// What follows the 1.12 `CMSG_CHAR_CREATE` body: Turtle's `HandleCharCreateOpcode` reads a
    /// trailing `u32` challenge mask (0 = no challenge mode) and kicks a body that ends early.
    pub fn char_create_tail(self) -> &'static [u8] {
        match self {
            ServerFlavor::Vanilla => &[],
            ServerFlavor::Turtle => &[0, 0, 0, 0],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_is_the_reference_wire() {
        let v = ServerFlavor::default();
        assert_eq!(v, ServerFlavor::Vanilla);
        assert_eq!(v.realmd_build(), 5875);
        assert!(!v.tolerates_warden());
        assert!(v.char_create_tail().is_empty());
    }

    #[test]
    fn turtle_presents_its_build_and_the_challenge_mask() {
        let t = ServerFlavor::Turtle;
        assert_eq!(t.realmd_build(), 7272);
        assert!(t.tolerates_warden());
        assert_eq!(t.char_create_tail(), &[0, 0, 0, 0]);
    }
}
