//! impit fork: the client's first SETTINGS frame as it goes on the wire.
//!
//! impit can't reach h2's builder (it goes through reqwest and hyper), so two env vars, read when a
//! client connection sends its first SETTINGS frame, change that frame:
//!
//! - `IMPIT_H2_OMIT_SETTINGS`: comma-separated setting ids to leave out, whitespace around each id
//!   ignored. Only 2 (ENABLE_PUSH), 5 (MAX_FRAME_SIZE) and 6 (MAX_HEADER_LIST_SIZE) are honoured;
//!   anything else is ignored.
//! - `IMPIT_H2_NO_RFC7540_PRIORITIES`: `0` or `1` adds setting 9 (SETTINGS_NO_RFC7540_PRIORITIES,
//!   RFC 9218 §2.1) with that value; anything else is ignored. Setting 9 is send-only.
//!
//! Unset or empty, the frame is the builder's settings, as before. Either way h2 keeps enforcing the
//! builder's settings: a left-out MAX_FRAME_SIZE means the initial 16384 to the server (RFC 9113
//! §6.5.2), and MAX_HEADER_LIST_SIZE is advisory, so h2 may enforce the builder's limit without
//! having sent it.
//!
//! One thing follows from the frame: with ENABLE_PUSH = 0 left out, the server may push (the initial
//! value is 1), so h2, which still has push disabled, refuses a pushed stream with RST_STREAM
//! REFUSED_STREAM (RFC 9113 §8.4.2) instead of closing the connection (`push_left_out`).

use crate::frame;

/// Comma-separated ids of the settings to leave out of the client's first SETTINGS frame.
pub(crate) const OMIT_SETTINGS_ENV: &str = "IMPIT_H2_OMIT_SETTINGS";

/// `0` or `1`: add SETTINGS_NO_RFC7540_PRIORITIES (id 9) with that value to that frame.
pub(crate) const NO_RFC7540_PRIORITIES_ENV: &str = "IMPIT_H2_NO_RFC7540_PRIORITIES";

/// The first SETTINGS frame to send for the builder's settings `local`, given the values of the two
/// variables (`None`: unset). `local` stays what h2 enforces.
pub(crate) fn wire_settings(
    local: &frame::Settings,
    omit: Option<&str>,
    no_rfc7540_priorities: Option<&str>,
) -> frame::Settings {
    let mut wire = local.clone();

    for id in omit.unwrap_or("").split(',').map(str::trim) {
        match id {
            "2" => wire.clear_enable_push(),
            "5" => wire.set_max_frame_size(None),
            "6" => wire.set_max_header_list_size(None),
            // 1, 3, 4 and 8 drive h2's own state, so they always go out as h2 has them; anything
            // else isn't a setting h2 sends.
            _ => {}
        }
    }

    match no_rfc7540_priorities {
        Some("0") => wire.set_no_rfc7540_priorities(Some(0)),
        Some("1") => wire.set_no_rfc7540_priorities(Some(1)),
        _ => {}
    }

    wire
}

/// [`wire_settings`] with the variables' current values (not valid Unicode counts as unset).
pub(crate) fn wire_settings_from_env(local: &frame::Settings) -> frame::Settings {
    let omit = std::env::var(OMIT_SETTINGS_ENV).ok();
    let no_rfc7540_priorities = std::env::var(NO_RFC7540_PRIORITIES_ENV).ok();
    wire_settings(local, omit.as_deref(), no_rfc7540_priorities.as_deref())
}

/// Whether `wire` left out an ENABLE_PUSH = 0 that `local` has. The server may then push (the
/// initial value is 1), although h2 has push disabled, so a pushed stream is refused instead of
/// closing the connection.
pub(crate) fn push_left_out(local: &frame::Settings, wire: &frame::Settings) -> bool {
    local.is_push_enabled() == Some(false) && wire.is_push_enabled().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    /// The client settings hyper's h2 builder sends.
    fn local() -> frame::Settings {
        let mut local = frame::Settings::default();
        local.set_header_table_size(Some(65_536));
        local.set_enable_push(false);
        local.set_initial_window_size(Some(6_291_456));
        local.set_max_frame_size(Some(16_384));
        local.set_max_header_list_size(Some(262_144));
        local
    }

    /// `local()` with `change` made to it.
    fn local_with(change: impl FnOnce(&mut frame::Settings)) -> frame::Settings {
        let mut settings = local();
        change(&mut settings);
        settings
    }

    /// `local()` plus the two settings hyper doesn't send, 3 (MAX_CONCURRENT_STREAMS) and 8
    /// (ENABLE_CONNECT_PROTOCOL): every setting h2 has.
    fn every_setting() -> frame::Settings {
        local_with(|s| {
            s.set_max_concurrent_streams(Some(100));
            s.set_enable_connect_protocol(Some(1));
        })
    }

    /// The wire settings for `local()` with `ids` left out.
    fn omit(ids: &str) -> frame::Settings {
        wire_settings(&local(), Some(ids), None)
    }

    #[test]
    fn wire_settings_unset_is_identity() {
        for settings in &[local(), frame::Settings::default(), every_setting()] {
            assert_eq!(&wire_settings(settings, None, None), settings);
            assert_eq!(&wire_settings(settings, Some(""), None), settings);
            assert_eq!(&wire_settings(settings, None, Some("")), settings);
            assert_eq!(&wire_settings(settings, Some(""), Some("")), settings);
        }
    }

    #[test]
    fn wire_settings_leaves_out_honoured_ids() {
        assert_eq!(omit("5").max_frame_size(), None);
        assert_eq!(omit("5"), local_with(|s| s.set_max_frame_size(None)));

        assert_eq!(omit("6").max_header_list_size(), None);
        assert_eq!(omit("6"), local_with(|s| s.set_max_header_list_size(None)));

        assert_eq!(omit("2").is_push_enabled(), None);
        assert_eq!(omit("2"), local_with(|s| s.clear_enable_push()));

        let both = omit("2,6");
        assert_eq!(both.is_push_enabled(), None);
        assert_eq!(both.max_header_list_size(), None);
        assert_eq!(
            both,
            local_with(|s| {
                s.clear_enable_push();
                s.set_max_header_list_size(None);
            })
        );
    }

    #[test]
    fn wire_settings_ignores_unknown_and_malformed_ids() {
        // Settings h2 needs in step with the server, ids h2 never sends, and junk: no change.
        for ids in &["1", "1,3,4,8", "7", "abc", "5;6", ",", " "] {
            assert_eq!(omit(ids), local(), "IMPIT_H2_OMIT_SETTINGS={:?}", ids);
        }
        // `local()` has no 3 or 8, so check those on settings that have them.
        assert_eq!(
            wire_settings(&every_setting(), Some("1,3,4,8"), None),
            every_setting()
        );

        // Repeats count once; whitespace around an id doesn't matter.
        assert_eq!(omit("5,5"), omit("5"));
        assert_eq!(omit(" 2"), omit("2"));
        assert_eq!(omit(" 5 , 6 "), omit("5,6"));
        assert_eq!(omit("5, 6"), omit("5,6"));

        // Unknown ids next to honoured ones don't stop them.
        assert_eq!(omit("1,5,7,abc"), omit("5"));
    }

    /// The wire settings for `local()` with IMPIT_H2_NO_RFC7540_PRIORITIES set to `value`.
    fn priorities(value: &str) -> frame::Settings {
        wire_settings(&local(), None, Some(value))
    }

    #[test]
    fn wire_settings_adds_setting_9() {
        assert_eq!(priorities("1").no_rfc7540_priorities(), Some(1));
        assert_eq!(
            priorities("1"),
            local_with(|s| s.set_no_rfc7540_priorities(Some(1)))
        );
        assert_eq!(priorities("0").no_rfc7540_priorities(), Some(0));

        for value in &["2", "yes", " 1", ""] {
            assert_eq!(
                priorities(value),
                local(),
                "IMPIT_H2_NO_RFC7540_PRIORITIES={:?}",
                value
            );
        }
    }

    /// The (id, value) pairs in an encoded SETTINGS frame, in order.
    fn encoded_pairs(settings: &frame::Settings) -> Vec<(u16, u32)> {
        let mut buf = BytesMut::new();
        settings.encode(&mut buf);
        let payload = &buf[frame::HEADER_LEN..];
        assert_eq!(payload.len() % 6, 0);
        payload
            .chunks(6)
            .map(|s| {
                let id = u16::from_be_bytes([s[0], s[1]]);
                let value = u32::from_be_bytes([s[2], s[3], s[4], s[5]]);
                (id, value)
            })
            .collect()
    }

    #[test]
    fn wire_settings_encoding_order() {
        let wire = wire_settings(&local(), Some("2,6"), Some("1"));
        assert_eq!(
            encoded_pairs(&wire),
            [(1, 65_536), (4, 6_291_456), (5, 16_384), (9, 1)]
        );
    }

    #[test]
    fn received_setting_9_is_ignored() {
        let mut buf = BytesMut::new();
        wire_settings(&local(), None, Some("1")).encode(&mut buf);
        let head = frame::Head::parse(&buf);
        let received = frame::Settings::load(head, &buf[frame::HEADER_LEN..]).expect("load");
        assert_eq!(received.no_rfc7540_priorities(), None);
        assert_eq!(received, local());
    }

    #[test]
    fn push_left_out_only_when_push_was_sent_as_zero() {
        assert!(push_left_out(&local(), &omit("2")));
        assert!(!push_left_out(&local(), &omit("5,6")));
        assert!(!push_left_out(
            &local(),
            &wire_settings(&local(), None, None)
        ));

        // Nothing sent, or push sent as enabled: leaving it out changes nothing for the server.
        for push in &[None, Some(true)] {
            let mut local = frame::Settings::default();
            if let Some(enable) = *push {
                local.set_enable_push(enable);
            }
            let wire = wire_settings(&local, Some("2"), None);
            assert!(!push_left_out(&local, &wire), "ENABLE_PUSH {:?}", push);
        }
    }
}
