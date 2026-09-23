// TODO: Support settign font settings
use std::collections::{BTreeSet, HashMap};
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
/// Where `items` already appear in `pool`, or where they now do after being appended.
fn share<T: PartialEq + Copy>(pool: &mut Vec<T>, items: &[T]) -> usize {
    if items.is_empty() {
        return 0;
    }
    if let Some(start) = pool.windows(items.len()).position(|w| w == items) {
        return start;
    }
    pool.extend_from_slice(items);
    pool.len() - items.len()
}

/// The glyphs as `PathGlyph`s over one static of points and one of contour ends, each glyph a
/// range of both, shared with any glyph whose values are the same. All three are named statics:
/// a linker script may place anonymous and mergeable constants in RAM, and esp-hal's does.
fn glyphs_to_tokens(glyphs: &[&Glyph]) -> proc_macro2::TokenStream {
    let (mut points, mut contours) = (Vec::new(), Vec::new());
    let glyphs = glyphs.iter().map(|glyph| {
        let bits: Vec<[u32; 2]> = glyph.points().iter().map(|p| p.map(f32::to_bits)).collect();
        let point_start = share(&mut points, &bits);
        let contour_start = share(&mut contours, glyph.contours());
        let (point_len, contour_len) = (bits.len(), glyph.contours().len());
        let (advance_width, advance_height) = (glyph.advance_width(), glyph.advance_height());
        let OutlineBounds {
            xmin,
            ymin,
            width,
            height,
        } = glyph.bounds();
        // SAFETY (of the emitted code): the points and bounds are a `Glyph`'s that the font
        // outlined, and each range holds exactly that glyph's values.
        quote! {
            unsafe {
                ::fontdue::PathGlyph::new(
                    range(&POINTS, #point_start, #point_len),
                    range(&CONTOURS, #contour_start, #contour_len),
                    ::fontdue::OutlineBounds {
                        xmin: #xmin,
                        ymin: #ymin,
                        width: #width,
                        height: #height,
                    },
                    #advance_width,
                    #advance_height,
                )
            }
        }
    });
    let glyphs = glyphs.collect::<Vec<_>>();
    let glyph_count = glyphs.len();
    let point_count = points.len();
    let contour_count = contours.len();
    let points = points.iter().map(|p| p.map(f32::from_bits)).map(|[x, y]| quote! { [#x, #y] });
    quote! {
        static POINTS: [[f32; 2]; #point_count] = [#(#points),*];
        static CONTOURS: [u32; #contour_count] = [#(#contours),*];
        const fn range<T>(all: &'static [T], start: usize, len: usize) -> &'static [T] {
            all.split_at(start).1.split_at(len).0
        }
        static GLYPHS: [::fontdue::PathGlyph<'static>; #glyph_count] = [#(#glyphs),*];
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
    let mut subset_chars = None;
    let mut store = false;
    let mut grid: Option<u32> = None;

    if tokens.clone().next().is_some_and(|t| matches!(t, TokenTree::Punct(punct) if punct.as_char() == ',')) {
        _ = tokens.next();
    }
    loop {
        match tokens.next() {
            None => break,
            Some(TokenTree::Punct(punct)) if punct.as_char() == ',' => continue,
            Some(TokenTree::Ident(i)) => match i.to_string().as_str() {
                "scale" => {
                    assert!(matches!(tokens.next(), Some(TokenTree::Punct(p)) if p.as_char() == ':'));
                    let TokenTree::Literal(lit) = tokens.next().unwrap() else {
                        panic!("Expected float literal to follow scale:")
                    };
                    settings.scale = lit.to_string().parse().unwrap();
                }
                "chars" => {
                    assert!(matches!(tokens.next(), Some(TokenTree::Punct(p)) if p.as_char() == ':'));
                    let TokenTree::Literal(lit) = tokens.next().unwrap() else {
                        panic!("Expected string literal to follow chars:")
                    };
                    // Through litrs rather than trimming the quotes off `to_string`, which leaves
                    // escapes as their source characters and takes `"\u{b0}"` to mean nine glyphs.
                    let lit = match litrs::StringLit::try_from(&lit) {
                        Ok(lit) => lit,
                        Err(err) => panic!("chars: expects a string literal: {err}"),
                    };
                    let mut chars = lit.value().chars().collect::<Vec<_>>();
                    chars.sort_unstable();
                    chars.dedup();
                    subset_chars = Some(chars);
                }
                "store" => {
                    assert!(matches!(tokens.next(), Some(TokenTree::Punct(p)) if p.as_char() == ':'));
                    store = match tokens.next() {
                        Some(TokenTree::Ident(b)) if b == "true" => true,
                        Some(TokenTree::Ident(b)) if b == "false" => false,
                        _ => panic!("Expected true or false to follow store:"),
                    };
                }
                "grid" => {
                    assert!(matches!(tokens.next(), Some(TokenTree::Punct(p)) if p.as_char() == ':'));
                    let TokenTree::Literal(lit) = tokens.next().unwrap() else {
                        panic!("Expected an integer literal to follow grid:")
                    };
                    let g: u32 = lit.to_string().parse().expect("grid: expects an integer, such as 16");
                    assert!(g.is_power_of_two() && g <= 1 << 16, "grid: must be a power of two up to 65536");
                    grid = Some(g);
                }
                _ => unimplemented!(),
            },
            _ => unimplemented!(),
        }
    }

    let font = fontdue::Font::from_bytes(ttf_data, settings).unwrap();
    let glyph_indices = match &subset_chars {
        None => (0..font.glyph_count()).collect::<Vec<_>>(),
        Some(chars) => {
            let mut indices = BTreeSet::from([0u16]);
            for character in chars {
                if let Some(index) = font.chars().get(character) {
                    indices.insert(index.get());
                }
            }
            indices.into_iter().collect()
        }
    };
    let glyph_remap: HashMap<u16, u16> =
        glyph_indices.iter().enumerate().map(|(new, old)| (*old, new as u16)).collect();

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
            let arms = map.iter().filter_map(|(key, value)| {
                let left = (*key >> 16) as u16;
                let right = *key as u16;
                let left = glyph_remap.get(&left)?;
                let right = glyph_remap.get(&right)?;
                let key = (u32::from(*left) << 16) | u32::from(*right);
                Some(quote! { #key => ::core::option::Option::Some(#value) })
            });
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
    let glyph_lookup_arms = match &subset_chars {
        None => Box::new(font.chars().iter().filter_map(|(k, v)| {
            let v = *glyph_remap.get(&v.get())?;
            Some(quote! { #k => #v })
        })) as Box<dyn Iterator<Item = _>>,
        Some(chars) => Box::new(chars.iter().filter_map(|k| {
            let v = *glyph_remap.get(&font.chars().get(k)?.get())?;
            Some(quote! { #k => #v })
        })),
    };
    let glyph_count = glyph_indices.len() as u16;
    assert!(store || grid.is_none(), "grid: only applies with store: true");
    let (glyph_items, glyph_methods, infos) = if store {
        store_items(&font, &glyph_indices, grid.unwrap_or(16), &type_name)
    } else {
        let glyphs: Vec<_> =
            glyph_indices.iter().map(|index| &font.internal_glyph_slice()[*index as usize]).collect();
        let infos = glyphs.iter().map(|glyph| fontdue::PathGlyph::from_glyph(glyph).info()).collect();
        let glyphs = glyphs_to_tokens(&glyphs);
        (
            quote! {},
            quote! {
                #[inline(always)]
                fn get_glyph_at_index(&self, index: u16) -> ::fontdue::GlyphRef<'_> {
                    #glyphs
                    GLYPHS[index as usize].into()
                }
            },
            infos,
        )
    };
    let capacity = raster_capacity_tokens(&infos, units_per_em);
    quote! {
        #glyph_items

        #[derive(Clone, Copy)]
        pub struct #type_name;

        impl #type_name {
            #capacity
        }

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

            #glyph_methods

            /// Gets the total glyphs in the font.
            #[inline(always)]
            fn glyph_count(&self) -> u16 {
                #glyph_count
            }
        }
    }
}

/// The store-backed half of a baked font: the store's tables as statics, and the `FontRepr`
/// methods that draw from them.
/// `raster_capacity` for glyphs with these outlines, from the largest diagonal among them.
fn raster_capacity_tokens(infos: &[fontdue::OutlineInfo], units_per_em: f32) -> proc_macro2::TokenStream {
    let diagonal = infos
        .iter()
        .map(|info| f64::from(info.bounds.width).hypot(f64::from(info.bounds.height)) * f64::from(info.unit))
        .fold(0.0, f64::max);
    // Rounded up, so the device's product is never below the exact one.
    let mut diagonal_f32 = diagonal as f32;
    if f64::from(diagonal_f32) < diagonal {
        diagonal_f32 = f32::from_bits(diagonal_f32.to_bits() + 1);
    }
    quote! {
        /// Raster buffer length, in f32s and including the three slack slots, that fits any glyph
        /// of this font at `px`, upright or under a transform that lengthens vectors by at most
        /// `stretch` (1 for a rotation), at any pen. It is at least what
        /// `fontdue::transformed_raster_capacity` gives for the same font, `px` and transform,
        /// and being `const` it can size a static buffer for `Raster::from_slice`.
        ///
        /// # Panics
        ///
        /// If `px` or `stretch` is not positive and finite, or the length overflows `usize`.
        pub const fn raster_capacity(px: f32, stretch: f32) -> usize {
            assert!(
                px > 0.0 && px <= f32::MAX && stretch > 0.0 && stretch <= f32::MAX,
                "px and stretch must be positive and finite"
            );
            let exact = stretch * (#diagonal_f32 * (px / #units_per_em));
            // The ceiling, by conversion: `f32::ceil` needs `std`. The conversion saturates, so
            // an infinite side fails the multiplication below.
            let whole = exact as usize;
            let side = if (whole as f32) < exact { whole.saturating_add(1) } else { whole };
            // One pixel more than `transformed_raster_capacity` adds, for the rounding that
            // differs between this product and its per-glyph one.
            let side = side.saturating_add(3);
            match side.checked_mul(side) {
                Some(area) if area <= usize::MAX - 3 => area + 3,
                _ => panic!("raster dimensions overflow usize"),
            }
        }
    }
}

fn store_items(
    font: &fontdue::Font,
    glyph_indices: &[u16],
    grid: u32,
    type_name: &proc_macro2::Ident,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream, Vec<fontdue::OutlineInfo>) {
    use fontdue::OutlineSource;
    use fontdue::store::{Store, encode};
    let shift = grid.trailing_zeros() as u8;
    let inputs: Vec<encode::GlyphInput> = glyph_indices
        .iter()
        .map(|&index| encode::glyph_input(&font.internal_glyph_slice()[index as usize], shift))
        .collect();
    let bytes = encode::encode(&inputs, shift);
    let words: Vec<u32> = bytes.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    // SAFETY: reinterpreting initialized u32s as their bytes, which is how the store reads them.
    let view = unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, 4 * words.len()) };
    // The full checked walk, here rather than on the device: every read inside the stream, every
    // point inside its glyph's bounds. The emitted store is exactly these parts.
    let store = Store::new(view).unwrap_or_else(|e| panic!("the encoded store failed its own check: {e:?}"));
    let p = store.parts();
    let (grid_value, id_bits, xy_bits, glyph_count, pool_count) =
        (p.grid, p.id_bits, p.xy_bits, p.glyph_count, p.pool_count);
    let (counts, bits, fast) = (p.step_counts, p.step_bits, p.step_fast);
    let (pool_offsets, glyphs, stream) = (p.pool_offsets, p.glyphs, p.words);
    let name = quote::format_ident!("__FONTDUE_STORE_{}", type_name);
    let items = quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        // SAFETY: these parts come from a store the macro built and checked with `Store::new`.
        static #name: ::fontdue::store::Store<'static> = unsafe {
            ::fontdue::store::Store::from_parts(::fontdue::store::Parts {
                grid: #grid_value,
                id_bits: #id_bits,
                xy_bits: #xy_bits,
                glyph_count: #glyph_count,
                pool_count: #pool_count,
                step_counts: [#(#counts),*],
                step_bits: &[#(#bits),*],
                step_fast: &[#(#fast),*],
                pool_offsets: &[#(#pool_offsets),*],
                glyphs: &[#(#glyphs),*],
                words: &[#(#stream),*],
            })
        };
    };
    let methods = quote! {
        #[inline]
        fn get_glyph_at_index(&self, index: u16) -> ::fontdue::GlyphRef<'_> {
            ::fontdue::GlyphRef::from_source(&#name, index)
        }

        #[inline]
        fn rasterize_indexed<'r>(
            &self,
            canvas: &'r mut ::fontdue::raster::Raster<'_>,
            index: u16,
            px: f32,
        ) -> (::fontdue::Metrics, ::fontdue::raster::BitmapIter<'r>) {
            ::fontdue::font::rasterize_source_indexed(canvas, &#name, index, px, self.scale_factor(px), 1.0)
        }

        #[inline]
        fn rasterize_indexed_subpixel<'r>(
            &self,
            canvas: &'r mut ::fontdue::raster::Raster<'_>,
            index: u16,
            px: f32,
        ) -> (::fontdue::Metrics, ::fontdue::raster::BitmapIter<'r>) {
            ::fontdue::font::rasterize_source_indexed(canvas, &#name, index, px, self.scale_factor(px), 3.0)
        }

        #[inline]
        fn rasterize_indexed_transformed<'r>(
            &self,
            canvas: &'r mut ::fontdue::raster::Raster<'_>,
            index: u16,
            px: f32,
            transform: ::fontdue::Transform,
            pen: (f32, f32),
        ) -> (::fontdue::TransformedMetrics, ::fontdue::raster::BitmapIter<'r>) {
            ::fontdue::rasterize_source_transformed_indexed(
                canvas,
                &#name,
                index,
                px,
                self.scale_factor(px),
                transform,
                pen,
            )
        }
    };
    let infos = (0..glyph_indices.len() as u16).map(|glyph| store.info(glyph)).collect();
    (items, methods, infos)
}
