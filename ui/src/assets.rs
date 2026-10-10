pub mod fonts {
    // Bundled font assets (bytes + family handles) wired up on demand by the
    // font registration path and the planned settings font picker; not every
    // family handle is referenced yet.
    #![allow(dead_code)]
    use iced::Font;

    // Geist's regular and italic variable faces share a family name so
    // cosmic-text can resolve style and weight without changing families.
    pub const GEIST_VF_BYTES: &[u8] = include_bytes!("../../assets/fonts/Geist[wght].ttf");
    pub const GEIST_ITALIC_VF_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/Geist-Italic[wght].ttf");
    pub const GEIST_VF: Font = Font::with_name("Geist");
    pub const GEIST_MONO_VF_BYTES: &[u8] = include_bytes!("../../assets/fonts/GeistMono[wght].ttf");
    pub const GEIST_MONO_ITALIC_VF_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/GeistMono-Italic[wght].ttf");
    pub const GEIST_MONO_VF: Font = Font::with_name("Geist Mono");
    pub const BOOTSTRAP_ICONS_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/bootstrap-icons.ttf");
    pub const BOOTSTRAP_ICONS: Font = Font::with_name("bootstrap-icons");

    // Monaspace variable fonts. The `with_name` strings are the family names
    // embedded in each TTF (name IDs 1 and 16 agree), verified against the
    // shipped files.
    pub const MONASPACE_ARGON_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/MonaspaceArgonVarVF.ttf");
    pub const MONASPACE_ARGON: Font = Font::with_name("Monaspace Argon Var");
    pub const MONASPACE_KRYPTON_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/MonaspaceKryptonVarVF.ttf");
    pub const MONASPACE_KRYPTON: Font = Font::with_name("Monaspace Krypton Var");
    pub const MONASPACE_NEON_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/MonaspaceNeonVarVF.ttf");
    pub const MONASPACE_NEON: Font = Font::with_name("Monaspace Neon Var");
    pub const MONASPACE_RADON_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/MonaspaceRadonVarVF.ttf");
    pub const MONASPACE_RADON: Font = Font::with_name("Monaspace Radon Var");
    pub const MONASPACE_XENON_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/MonaspaceXenonVarVF.ttf");
    pub const MONASPACE_XENON: Font = Font::with_name("Monaspace Xenon Var");

    // Additional bundled terminal fonts. Courier Prime and Fira Mono ship a
    // separate file per weight/style under one family name: every face is
    // registered so the terminal can resolve bold/italic, but the picker
    // offers a single family entry each. The `with_name` strings are the
    // family names embedded in the files (name ID 1), asserted below.
    pub const COURIER_PRIME_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/CourierPrime-Regular.ttf");
    pub const COURIER_PRIME_BOLD_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/CourierPrime-Bold.ttf");
    pub const COURIER_PRIME_ITALIC_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/CourierPrime-Italic.ttf");
    pub const COURIER_PRIME_BOLD_ITALIC_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/CourierPrime-BoldItalic.ttf");
    pub const COURIER_PRIME: Font = Font::with_name("Courier Prime");

    pub const DEPARTURE_MONO_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/DepartureMono-Regular.otf");
    pub const DEPARTURE_MONO: Font = Font::with_name("Departure Mono");

    pub const FIRA_MONO_BYTES: &[u8] = include_bytes!("../../assets/fonts/FiraMono-Regular.ttf");
    pub const FIRA_MONO_MEDIUM_BYTES: &[u8] =
        include_bytes!("../../assets/fonts/FiraMono-Medium.ttf");
    pub const FIRA_MONO_BOLD_BYTES: &[u8] = include_bytes!("../../assets/fonts/FiraMono-Bold.ttf");
    pub const FIRA_MONO: Font = Font::with_name("Fira Mono");

    pub const LILEX_BYTES: &[u8] = include_bytes!("../../assets/fonts/Lilex[wght].ttf");
    pub const LILEX: Font = Font::with_name("Lilex");

    pub const VT323_BYTES: &[u8] = include_bytes!("../../assets/fonts/VT323-Regular.ttf");
    pub const VT323: Font = Font::with_name("VT323");

    pub const FIXEDSYS_EX_BYTES: &[u8] = include_bytes!("../../assets/fonts/FSEX302.ttf");
    pub const FIXEDSYS_EX: Font = Font::with_name("Fixedsys Excelsior");

    #[cfg(test)]
    mod tests {
        /// The `Font::with_name` strings (and the family names offered in the
        /// settings font picker) must match the family names embedded in the
        /// bundled TTFs, or cosmic-text silently falls back to another font.
        /// fontdb is what iced/cosmic-text resolve families with, so assert
        /// through it.
        #[test]
        fn bundled_font_family_names_match_ttf_metadata() {
            let cases: &[(&[u8], &str)] = &[
                (super::GEIST_VF_BYTES, "Geist"),
                (super::GEIST_ITALIC_VF_BYTES, "Geist"),
                (super::GEIST_MONO_VF_BYTES, "Geist Mono"),
                (super::GEIST_MONO_ITALIC_VF_BYTES, "Geist Mono"),
                (super::BOOTSTRAP_ICONS_BYTES, "bootstrap-icons"),
                (super::MONASPACE_ARGON_BYTES, "Monaspace Argon Var"),
                (super::MONASPACE_KRYPTON_BYTES, "Monaspace Krypton Var"),
                (super::MONASPACE_NEON_BYTES, "Monaspace Neon Var"),
                (super::MONASPACE_RADON_BYTES, "Monaspace Radon Var"),
                (super::MONASPACE_XENON_BYTES, "Monaspace Xenon Var"),
                (super::COURIER_PRIME_BYTES, "Courier Prime"),
                (super::DEPARTURE_MONO_BYTES, "Departure Mono"),
                (super::FIRA_MONO_BYTES, "Fira Mono"),
                (super::LILEX_BYTES, "Lilex"),
                (super::VT323_BYTES, "VT323"),
                (super::FIXEDSYS_EX_BYTES, "Fixedsys Excelsior"),
            ];

            for (bytes, expected) in cases {
                let mut db = fontdb::Database::new();
                db.load_font_data(bytes.to_vec());
                let families: Vec<&str> = db
                    .faces()
                    .flat_map(|face| face.families.iter().map(|(name, _)| name.as_str()))
                    .collect();
                assert!(
                    families.contains(expected),
                    "expected family {expected:?}, TTF reports {families:?}"
                );
            }
        }

        #[test]
        fn geist_faces_have_the_expected_style_and_spacing_metadata() {
            let cases: &[(&[u8], fontdb::Style, bool)] = &[
                (super::GEIST_VF_BYTES, fontdb::Style::Normal, false),
                (super::GEIST_ITALIC_VF_BYTES, fontdb::Style::Italic, false),
                (super::GEIST_MONO_VF_BYTES, fontdb::Style::Normal, true),
                (
                    super::GEIST_MONO_ITALIC_VF_BYTES,
                    fontdb::Style::Italic,
                    true,
                ),
            ];

            for (bytes, expected_style, expected_monospaced) in cases {
                let mut db = fontdb::Database::new();
                db.load_font_data(bytes.to_vec());
                let faces: Vec<_> = db.faces().collect();
                assert_eq!(faces.len(), 1, "expected one face, got {faces:?}");
                assert_eq!(faces[0].style, *expected_style);
                assert_eq!(faces[0].monospaced, *expected_monospaced);
            }
        }

        /// Unpatched cosmic-text 0.15 rejects a variable face when the
        /// requested weight differs from the face's default; guards the
        /// `variable_weight_match` backport in
        /// `patches/cosmic-text+0.15.0.patch`.
        #[test]
        fn geist_mono_variable_weights_keep_the_requested_family() {
            use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight};

            let mut db = fontdb::Database::new();
            for bytes in [
                super::GEIST_VF_BYTES,
                super::GEIST_ITALIC_VF_BYTES,
                super::GEIST_MONO_VF_BYTES,
                super::GEIST_MONO_ITALIC_VF_BYTES,
            ] {
                db.load_font_data(bytes.to_vec());
            }
            let mut font_system = FontSystem::new_with_locale_and_db("en-US".into(), db);

            for (style, expected_style) in [
                (cosmic_text::Style::Normal, fontdb::Style::Normal),
                (cosmic_text::Style::Italic, fontdb::Style::Italic),
            ] {
                for weight in [100, 400, 700, 900] {
                    let mut buffer = Buffer::new(&mut font_system, Metrics::new(16.0, 20.0));
                    let glyph_font_ids;
                    {
                        let mut buffer = buffer.borrow_with(&mut font_system);
                        let attrs = Attrs::new()
                            .family(Family::Name("Geist Mono"))
                            .style(style)
                            .weight(Weight(weight));
                        buffer.set_size(Some(300.0), Some(100.0));
                        buffer.set_text("Bold terminal text", &attrs, Shaping::Advanced, None);
                        buffer.shape_until_scroll(true);
                        glyph_font_ids = buffer
                            .layout_runs()
                            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.font_id))
                            .collect::<Vec<_>>();
                    }

                    assert!(
                        !glyph_font_ids.is_empty(),
                        "weight {weight}: no glyphs produced"
                    );
                    for id in glyph_font_ids {
                        let face = font_system
                            .db()
                            .face(id)
                            .expect("shaped face is registered");
                        assert!(
                            face.families
                                .iter()
                                .any(|(family, _)| family == "Geist Mono"),
                            "weight {weight}: expected Geist Mono, got {:?}",
                            face.families
                        );
                        assert_eq!(face.style, expected_style, "weight {weight}");
                        assert!(
                            face.monospaced,
                            "weight {weight}: selected a proportional face"
                        );
                    }
                }
            }
        }
    }
}

pub mod bootstrap_icons {
    // Bundled Bootstrap Icons glyph codepoints; the full set is kept available
    // for use on demand, so some entries aren't referenced yet.
    #![allow(dead_code)]
    pub const ARROW_CLOCKWISE: &str = "\u{F116}";
    pub const ARROW_COUNTERCLOCKWISE: &str = "\u{F117}";
    pub const ARROW_REPEAT: &str = "\u{F130}";
    pub const ASTERISK: &str = "\u{F151}";
    pub const AT: &str = "\u{F152}";
    pub const BOUNDING_BOX: &str = "\u{F1B6}";
    pub const CHECK_2: &str = "\u{F272}";
    pub const CHEVRON_DOWN: &str = "\u{F282}";
    pub const CHEVRON_RIGHT: &str = "\u{F285}";
    pub const CHEVRON_UP: &str = "\u{F286}";
    pub const CLOUD_CHECK: &str = "\u{F299}";
    pub const CLOUD_UPLOAD: &str = "\u{F2C0}";
    pub const CROSSHAIR: &str = "\u{F769}";
    pub const CURSOR: &str = "\u{F2E3}";
    pub const DATABASE: &str = "\u{F8C4}";
    pub const DPAD: &str = "\u{F687}";
    pub const EXCLAMATION_TRIANGLE: &str = "\u{F33B}";
    pub const EYE_SLASH: &str = "\u{F340}";
    pub const FOLDER_PLUS: &str = "\u{F3D3}";
    pub const FONTS: &str = "\u{F3DA}";
    pub const LIGHTNING: &str = "\u{F46F}";
    pub const LIST_UL: &str = "\u{F478}";
    pub const PENCIL: &str = "\u{F4CB}";
    pub const PEOPLE: &str = "\u{F4D9}";
    pub const PLUS_LG: &str = "\u{F64D}";
    pub const PLUS_SQUARE: &str = "\u{F4FD}";
    pub const SEARCH: &str = "\u{F52A}";
    pub const SHARE: &str = "\u{F52E}";
    pub const SLASH_CIRCLE: &str = "\u{F567}";
    pub const TOGGLE_OFF: &str = "\u{F5D5}";
    pub const TOGGLE_ON: &str = "\u{F5D6}";
    pub const TRASH_3: &str = "\u{F78B}";
}

pub mod hero_icons {
    use std::sync::LazyLock;

    use iced::widget::svg;
    pub const BARS_3_BYTES: &[u8] =
        include_bytes!("../../assets/heroicons/optimized/16/solid/bars-3.svg");

    pub static BARS_3: LazyLock<svg::Handle> =
        LazyLock::new(|| svg::Handle::from_memory(BARS_3_BYTES));

    pub const MINUS_BYTES: &[u8] =
        include_bytes!("../../assets/heroicons/optimized/16/solid/minus.svg");

    pub static MINUS: LazyLock<svg::Handle> =
        LazyLock::new(|| svg::Handle::from_memory(MINUS_BYTES));

    pub const STOP_BYTES: &[u8] =
        include_bytes!("../../assets/heroicons/optimized/16/solid/stop.svg");

    pub static STOP: LazyLock<svg::Handle> = LazyLock::new(|| svg::Handle::from_memory(STOP_BYTES));

    pub const SQUARE_2_STACK_BYTES: &[u8] =
        include_bytes!("../../assets/heroicons/optimized/16/solid/square-2-stack.svg");

    pub static SQUARE_2_STACK: LazyLock<svg::Handle> =
        LazyLock::new(|| svg::Handle::from_memory(SQUARE_2_STACK_BYTES));

    pub const X_MARK_BYTES: &[u8] =
        include_bytes!("../../assets/heroicons/optimized/16/solid/x-mark.svg");

    pub static X_MARK: LazyLock<svg::Handle> =
        LazyLock::new(|| svg::Handle::from_memory(X_MARK_BYTES));
}
