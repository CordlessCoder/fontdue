// TODO: Support settign font settings
use std::path::{Path, PathBuf};

use fontdue::{FontRepr, LineMetrics, OutlineBounds, font::Glyph};
use proc_macro2::TokenTree;
use quote::{ToTokens, quote};

/// fontdue_font_from_file!(StaticFontName, "path/to/font.ttf") => {
///     pub struct StaticFontName;
/// }
#[proc_macro]
pub fn fontdue_font_from_file(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let span = proc_macro::Span::call_site();
    let path = span.local_file().unwrap();
    fontdue_font_from_file_impl(input.into(), path).into()
}

fn line_metrics_to_tokens(lm: &LineMetrics) -> proc_macro2::TokenStream {
    let LineMetrics {
        ascent,
        descent,
        line_gap,
        new_line_size,
    } = lm;
    quote! {
        ::fontdue::LineMetrics {
            ascent: #ascent,
            descent: #descent,
            line_gap: #line_gap,
            new_line_size: #new_line_size
        }
    }
}
fn f32x4_to_tokens(val: &fontdue::math::f32x4) -> proc_macro2::TokenStream {
    let (a, b, c, d) = val.copied();
    quote! {
        ::fontdue::math::f32x4::new(#a, #b, #c, #d)
    }
}
fn line_to_tokens(line: &fontdue::math::Line) -> proc_macro2::TokenStream {
    let fontdue::math::Line {
        coords,
        nudge,
        adjustment,
        params,
    } = line;
    let [coords, nudge, adjustment, params] = [coords, nudge, adjustment, params].map(f32x4_to_tokens);
    quote! {
        ::fontdue::math::Line {
            coords: #coords,
            nudge: #nudge,
            adjustment: #adjustment,
            params: #params
        }
    }
}
fn glyph_to_tokens(glyph: &Glyph) -> proc_macro2::TokenStream {
    let Glyph {
        v_lines,
        m_lines,
        bounds,
        advance_width,
        advance_height,
    } = glyph;
    let OutlineBounds {
        xmin,
        ymin,
        width,
        height,
    } = bounds;

    let v_lines = v_lines.iter().map(line_to_tokens);
    let m_lines = m_lines.iter().map(line_to_tokens);
    quote! {
        ::fontdue::font::GlyphRef {
            v_lines: &[#(#v_lines),*],
            m_lines: &[#(#m_lines),*],
            bounds: ::fontdue::font::OutlineBounds {
                xmin: #xmin,
                ymin: #ymin,
                width: #width,
                height: #height,
            },
            advance_width: #advance_width,
            advance_height: #advance_height,
        }
    }
}

fn some(val: impl ToTokens) -> proc_macro2::TokenStream {
    quote! {
        ::core::option::Option::Some(#val)
    }
}

fn fontdue_font_from_file_impl(
    input: proc_macro2::TokenStream,
    mut source: PathBuf,
) -> proc_macro2::TokenStream {
    let mut tokens = input.into_iter();
    let Some(TokenTree::Ident(type_name)) = tokens.next() else {
        panic!("Expected name of new font type as the first argument.");
    };
    if !matches!(tokens.next(), Some(TokenTree::Punct(punct)) if punct.as_char() == ',') {
        panic!("Expected name of new font type to be followed by a comma ");
    };
    let Some(TokenTree::Literal(l)) = tokens.next() else {
        panic!("Expected string literal of the font file's path as the first argument.");
    };
    let l = l.to_string();
    let Some(path) = l.strip_prefix('"').and_then(|l| l.strip_suffix('"')) else {
        panic!("Expected first argument to be the font file path as a string literal.")
    };
    source.pop();
    source.extend(Path::new(path).components());
    let path = source;
    let ttf_data = std::fs::read(path).unwrap();
    let mut settings = fontdue::FontSettings::default();

    if tokens.clone().next().is_some_and(|t| matches!(t, TokenTree::Punct(punct) if punct.as_char() == ',')) {
        _ = tokens.next();
    }
    loop {
        match tokens.next() {
            None => break,
            Some(TokenTree::Ident(i)) => match i.to_string().as_str() {
                "scale" => {
                    assert!(matches!(tokens.next(), Some(TokenTree::Punct(p)) if p.as_char() == ':'));
                    let TokenTree::Literal(lit) = tokens.next().unwrap() else {
                        panic!("Expected float literal to follow scale:")
                    };
                    settings.scale = lit.to_string().parse().unwrap();
                }
                _ => unimplemented!(),
            },
            _ => unimplemented!(),
        }
    }

    let font = fontdue::Font::from_bytes(ttf_data, settings).unwrap();

    let none = || {
        quote! {
            ::core::option::Option::None
        }
    };
    let name = match font.name() {
        None => none(),
        Some(name) => some(name),
    };
    let hash = font.file_hash();
    let vmetrics = match font.vertical_line_metrics_em() {
        None => none(),
        Some(lm) => some(line_metrics_to_tokens(&lm)),
    };
    let hmetrics = match font.horizontal_line_metrics_em() {
        None => none(),
        Some(lm) => some(line_metrics_to_tokens(&lm)),
    };
    let units_per_em = font.units_per_em();
    let horizontal_kern = match font.internal_horizontal_kern_map() {
        None => none(),
        Some(map) => {
            let arms = map.iter().map(|(k, v)| quote! { #k => ::core::option::Option::Some(#v) });
            quote! {
                let scale = self.scale_factor(px);
                let key = u32::from(left) << 16 | u32::from(right);
                let value = match key {
                    #(#arms,)*
                    _ => None,
                };
                value.map(|value| value as f32 * scale)
            }
        }
    };
    let glyph_lookup_arms = font.chars().iter().map(|(k, v)| {
        let v = v.get();
        quote! { #k => #v }
    });
    let glyph_count = font.glyph_count();
    let glyphs = font.internal_glyph_slice().iter().map(glyph_to_tokens);
    quote! {
        #[derive(Clone, Copy)]
        pub struct #type_name;

        impl ::fontdue::FontRepr for #type_name {
            #[inline(always)]
            fn name(&self) -> Option<&str> {
                #name
            }

            #[inline(always)]
            fn file_hash(&self) -> usize {
                #hash
            }

            #[inline]
            fn horizontal_line_metrics_em(&self) -> Option<::fontdue::LineMetrics> {
                #hmetrics
            }

            #[inline]
            fn vertical_line_metrics_em(&self) -> Option<::fontdue::LineMetrics> {
                #vmetrics
            }

            #[inline]
            fn units_per_em(&self) -> f32 {
                #units_per_em
            }

            #[inline]
            fn horizontal_kern_indexed(&self, left: u16, right: u16, px: f32) -> Option<f32> {
                #horizontal_kern
            }

            /// Finds the internal glyph index for the given character. If the character is not present in
            /// the font then 0 is returned.
            #[inline]
            fn lookup_glyph_index(&self, character: char) -> u16 {
                match character {
                    #(#glyph_lookup_arms,)*
                    _ => 0,
                }
            }

            #[inline(always)]
            fn get_glyph_at_index(&self, index: u16) -> ::fontdue::font::GlyphRef<'_> {
                static GLYPHS: &'static [::fontdue::font::GlyphRef<'static>] = &[
                    #(#glyphs),*
                ];
                GLYPHS[index as usize]
            }

            /// Gets the total glyphs in the font.
            #[inline(always)]
            fn glyph_count(&self) -> u16 {
                #glyph_count
            }
        }
    }
}
