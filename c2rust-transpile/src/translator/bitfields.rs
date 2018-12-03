#![deny(missing_docs)]
//! This module provides translation for bitfield structs and operations on them. Generated code
//! requires the use of the bitfield crate.

// TODO: See if union bitfields are supportable or not
// FIXME: size_of won't call the bitfield struct with required generic params, same
// for variable bindings: `let foo: Foo` should be `let foo: Foo<[u8; X]>`
// TODO: We can generate static bitfields in a slightly less nice, but safe, way:
// static mut bitfield: BitField<[u8; X]> = BitField([A, B, C, D]); Either we figure
// out what bits to assign here, or else we reuse the local var version but ensure that
// static bitfields get sectioned off

use c_ast::{CExprId, CQualTypeId};
use c2rust_ast_builder::mk;
use syntax::ast::{AttrStyle, Expr, MetaItemKind, NestedMetaItem, NestedMetaItemKind, Lit, LitIntType, LitKind, StrStyle, Ty};
use syntax::ext::quote::rt::Span;
use syntax::ptr::P;
use syntax::source_map::symbol::Symbol;
use syntax_pos::DUMMY_SP;
use translator::{ExprContext, Translation, ConvertedDecl, simple_metaitem};
use with_stmts::WithStmts;

/// (name, bitfield_width, platform_bit_offset, platform_type_bitwidth)
type NameWidthOffset = (String, P<Ty>, Option<u64>, u64, u64);
/// (name, type, bitfield_width, platform_type_bitwidth)
type NameTypeWidth = (String, CQualTypeId, Option<u64>, u64);

fn assigment_metaitem(lhs: &str, rhs: &str) -> NestedMetaItem {
    let meta_item = mk().meta_item(
        vec![lhs],
        MetaItemKind::NameValue(Lit {
            span: DUMMY_SP,
            node: LitKind::Str(Symbol::intern(rhs), StrStyle::Cooked)
        }),
    );

    mk().nested_meta_item(NestedMetaItemKind::MetaItem(meta_item))
}

/// Figure out what's the smallest byte int that can hold this bitfield width
/// This is required by packed_struct as you cannot currently use arbitrary sized
/// ints for fields like C lets you do..
fn get_ty_from_bit_width(bit_width: u64) -> Result<P<Ty>, &'static str> {
    // REVIEW: What about signed int fields?
    match bit_width {
        0..=8 => Ok(mk().ident_ty("u8")),
        9..=16 => Ok(mk().ident_ty("u16")),
        17..=32 => Ok(mk().ident_ty("u32")),
        33..=64 => Ok(mk().ident_ty("u64")),
        65..=128 => Ok(mk().ident_ty("u128")),
        _ => Err("Unsupported bitfield width found greater than 128 bits"),
    }
}

impl<'a> Translation<'a> {
    /// Here we output a macro invocation to generate bitfield data that looks like this:
    ///
    /// ```no_run
    /// bitfield! {
    ///     #[repr(C)]
    ///     #[derive(Copy, Clone)]
    ///     pub struct Foo([u8]);
    ///     pub int_size, field, set_field: end_idx, start_idx;
    ///     pub int_size2, field2, set_field2: end_idx2, start_idx2;
    ///     // ...
    /// }
    /// ```
    /// FIXME
    pub fn convert_bitfield_struct_decl(
        &self,
        name: String,
        platform_alignment: u64,
        span: Span,
        field_info: Vec<NameWidthOffset>,
    ) -> Result<ConvertedDecl, String> {
        self.extern_crates.borrow_mut().insert("c2rust_bitfields");

        let mut item_store = self.item_store.borrow_mut();

        item_store.uses
            .get_mut(vec!["c2rust_bitfields".into()])
            .insert("BitfieldStruct");

        let mut field_entries = Vec::with_capacity(field_info.len());

        for (field_name, ty, bitfield_width, bit_index, platform_ty_bitwidth) in field_info {
            let bitfield_width = match bitfield_width {
                // Bitfield widths of 0 should just be markers for clang,
                // we shouldn't need to explicitly handle it ourselves
                Some(0) => continue,
                None => {
                    let field = mk().struct_field(field_name, ty);

                    field_entries.push(field);
                    continue;
                },
                Some(bw) => bw,
            };

            // REVIEW: If a bitfield is unnamed (even if non 0), can we skip it?

            // FIXME: len dep on total compressed size
            let base_ty = mk().array_ty(
                mk().ident_ty("u8"),
                mk().lit_expr(mk().int_lit(bitfield_width as u128 / 8 + 1, LitIntType::Unsuffixed)),
            );
            // let field_generic_tys = mk().angle_bracketed_args(vec![base_ty, mk().ident_ty(bit_struct_name.clone())]);
            // let field_ty = mk().path_segment_with_args("Integer", field_generic_tys);
            let bit_range = format!("{}..={}", bit_index, bit_index + bitfield_width - 1);
            let field_attr_items = vec![
                assigment_metaitem("name", "FIXME"),
                assigment_metaitem("ty", "FIXME"),
                assigment_metaitem("bits", &bit_range),
            ];
            // TODO: May need more than one attr on the field
            let field_attr = mk().meta_item("bitfield", MetaItemKind::List(field_attr_items));
            let field = mk()
                .meta_item_attr(AttrStyle::Outer, field_attr)
                // .struct_field(field_name, mk().path_ty(vec![field_ty]));
                .struct_field(field_name, base_ty);

            field_entries.push(field);
        }

        let repr_items = vec![
            simple_metaitem("C"),
            simple_metaitem(&format!("align({})", platform_alignment)),
        ];
        let repr_attr = mk().meta_item("repr", MetaItemKind::List(repr_items));

        let item = mk()
            .span(span)
            .pub_()
            .call_attr("derive", vec!["Copy", "Clone", "BitfieldStruct"])
            .meta_item_attr(AttrStyle::Outer, repr_attr)
            .struct_item(name, field_entries);

        Ok(ConvertedDecl::Item(item))
    }

    /// Here we output a block to generate a struct literal initializer in.
    /// It looks like this in locals:
    ///
    /// FIXME
    ///
    /// ```no_run
    /// {
    ///     let mut foo = Foo([0; PLATFORM_BYTE_SIZE]);
    ///     foo.set_field(123);
    ///     foo.set_field2(456);
    ///     // ...
    ///     foo
    /// }
    /// ```
    ///
    /// and in statics:
    /// TODO
    pub fn convert_bitfield_struct_literal(&self, name: String, field_ids: &[CExprId], field_info: Vec<NameTypeWidth>, platform_alignment: u64, ctx: ExprContext) -> Result<WithStmts<P<Expr>>, String> {
        // REVIEW: statics?
        let mut stmts = Vec::with_capacity(field_ids.len());
        let mut fields = Vec::with_capacity(field_ids.len());

        // Specified record fields
        for (&field_id, (field_name, _, bitfield_width, platform_type_bitwidth)) in field_ids.iter().zip(field_info.iter()) {
            // Bitfield widths of 0 should just be markers for clang,
            // we shouldn't need to explicitly handle it ourselves
            if let Some(0) = bitfield_width {
                continue;
            }

            let bitwidth = bitfield_width.unwrap_or(*platform_type_bitwidth);
            // With some type analysis, we could skip this cast sometimes
            let cast_ty = get_ty_from_bit_width(bitwidth)?;

            let mut expr = self.convert_expr(ctx, field_id)?;

            stmts.append(&mut expr.stmts);

            let cast = mk().cast_expr(expr.val, cast_ty);
            // let field = mk().method_call_expr(cast, "into", Vec::new() as Vec<P<Expr>>);

            fields.push(mk().field(field_name, cast));
        }

        // Pad out remaining omitted record fields
        for (field_name, ty, _, _) in field_info[field_ids.len()..].iter() {
            fields.push(mk().field(field_name, self.implicit_default_expr(ty.ctype, ctx.is_static)?));
        }

        Ok(WithStmts {
            stmts,
            val: mk().struct_expr(vec![mk().path_segment(name)], fields),
        })
    }
}
