//! IA2 ABI pinned to LinuxA11y/IAccessible2 c9ae003d9c85eb707716928de97e055f5b77189c.
//! Upstream notices are preserved in the crate-root IA2-LICENSE.
#![allow(
    non_snake_case,
    dead_code,
    reason = "IDL spellings and unused ABI slots preserve the native vtable layout."
)]
//! Exact IDL method order; IDL boolean is u8, not Win32 BOOL.
use std::ffi::c_void;
use windows::core::{IUnknown, IUnknown_Vtbl, BSTR, HRESULT};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::IDispatch;
use windows::Win32::UI::Accessibility::{IAccessible, IAccessible_Vtbl};
type LongOut = unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT;
type StringOut = unsafe extern "system" fn(*mut c_void, *mut BSTR) -> HRESULT;

#[repr(C)]
pub struct Locale {
    pub language: BSTR,
    pub country: BSTR,
    pub variant: BSTR,
}

// The unused slots are deliberately retained with their exact IDL signatures:
// removing one changes every subsequent native method's offset.
#[repr(C)]
pub struct Accessible2Vtable {
    pub base: IAccessible_Vtbl,
    pub n_relations: LongOut,
    pub relation: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT,
    pub relations:
        unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void, *mut i32) -> HRESULT,
    pub role: LongOut,
    pub scroll_to: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub scroll_to_point: unsafe extern "system" fn(*mut c_void, i32, i32, i32) -> HRESULT,
    pub group_position:
        unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32, *mut i32) -> HRESULT,
    pub states: LongOut,
    pub extended_role: StringOut,
    pub localized_extended_role: StringOut,
    pub n_extended_states: LongOut,
    pub extended_states:
        unsafe extern "system" fn(*mut c_void, i32, *mut *mut BSTR, *mut i32) -> HRESULT,
    pub localized_extended_states:
        unsafe extern "system" fn(*mut c_void, i32, *mut *mut BSTR, *mut i32) -> HRESULT,
    pub unique_id: LongOut,
    pub window_handle: unsafe extern "system" fn(*mut c_void, *mut HWND) -> HRESULT,
    pub index_in_parent: LongOut,
    pub locale: unsafe extern "system" fn(*mut c_void, *mut Locale) -> HRESULT,
    pub attributes: StringOut,
}
windows::core::imp::define_interface!(
    Accessible2,
    Accessible2Vtable,
    0xe89f726e_c4f4_4c19_bb19_b647d7fa8478
);
windows::core::imp::interface_hierarchy!(Accessible2, IUnknown, IDispatch, IAccessible);

#[repr(C)]
pub struct AccessibleRelationVtable {
    pub base: IUnknown_Vtbl,
    pub relationType: StringOut,
    pub localizedRelationType: StringOut,
    pub nTargets: LongOut,
    pub target: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT,
    pub targets: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void, *mut i32) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleRelation,
    AccessibleRelationVtable,
    0x7cdf86ee_c3da_496a_bda4_281b336e1fdc
);
windows::core::imp::interface_hierarchy!(AccessibleRelation, IUnknown);

#[repr(C)]
pub struct AccessibleHyperlinkVtable {
    pub base: AccessibleActionVtable,
    pub anchor: unsafe extern "system" fn(
        *mut c_void,
        i32,
        *mut windows::Win32::System::Variant::VARIANT,
    ) -> HRESULT,
    pub anchorTarget: unsafe extern "system" fn(
        *mut c_void,
        i32,
        *mut windows::Win32::System::Variant::VARIANT,
    ) -> HRESULT,
    pub startIndex: LongOut,
    pub endIndex: LongOut,
    pub valid: unsafe extern "system" fn(*mut c_void, *mut u8) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleHyperlink,
    AccessibleHyperlinkVtable,
    0x01c20f2b_3dd2_400f_949f_ad00bdab1d41
);
windows::core::imp::interface_hierarchy!(AccessibleHyperlink, IUnknown, AccessibleAction);

#[repr(C)]
pub struct TextSegment {
    pub text: BSTR,
    pub start: i32,
    pub end: i32,
}
#[repr(C)]
pub struct TableChange {
    pub kind: i32,
    pub first_row: i32,
    pub last_row: i32,
    pub first_column: i32,
    pub last_column: i32,
}

#[repr(C)]
pub struct AccessibleTextVtable {
    pub base: IUnknown_Vtbl,
    pub addSelection: unsafe extern "system" fn(*mut c_void, i32, i32) -> HRESULT,
    pub attributes:
        unsafe extern "system" fn(*mut c_void, i32, *mut i32, *mut i32, *mut BSTR) -> HRESULT,
    pub caretOffset: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub characterExtents: unsafe extern "system" fn(
        *mut c_void,
        i32,
        i32,
        *mut i32,
        *mut i32,
        *mut i32,
        *mut i32,
    ) -> HRESULT,
    pub nSelections: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub offsetAtPoint: unsafe extern "system" fn(*mut c_void, i32, i32, i32, *mut i32) -> HRESULT,
    pub selection: unsafe extern "system" fn(*mut c_void, i32, *mut i32, *mut i32) -> HRESULT,
    pub text: unsafe extern "system" fn(*mut c_void, i32, i32, *mut BSTR) -> HRESULT,
    pub textBeforeOffset:
        unsafe extern "system" fn(*mut c_void, i32, i32, *mut i32, *mut i32, *mut BSTR) -> HRESULT,
    pub textAfterOffset:
        unsafe extern "system" fn(*mut c_void, i32, i32, *mut i32, *mut i32, *mut BSTR) -> HRESULT,
    pub textAtOffset:
        unsafe extern "system" fn(*mut c_void, i32, i32, *mut i32, *mut i32, *mut BSTR) -> HRESULT,
    pub removeSelection: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub setCaretOffset: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub setSelection: unsafe extern "system" fn(*mut c_void, i32, i32, i32) -> HRESULT,
    pub nCharacters: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub scrollSubstringTo: unsafe extern "system" fn(*mut c_void, i32, i32, i32) -> HRESULT,
    pub scrollSubstringToPoint:
        unsafe extern "system" fn(*mut c_void, i32, i32, i32, i32, i32) -> HRESULT,
    pub newText: unsafe extern "system" fn(*mut c_void, *mut TextSegment) -> HRESULT,
    pub oldText: unsafe extern "system" fn(*mut c_void, *mut TextSegment) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleText,
    AccessibleTextVtable,
    0x24fd2ffb3aad4a088335a3ad89c0fb4b
);
windows::core::imp::interface_hierarchy!(AccessibleText, IUnknown);

#[repr(C)]
pub struct AccessibleHypertextVtable {
    pub base: AccessibleTextVtable,
    pub nHyperlinks: LongOut,
    pub hyperlink: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT,
    pub hyperlinkIndex: unsafe extern "system" fn(*mut c_void, i32, *mut i32) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleHypertext,
    AccessibleHypertextVtable,
    0x6b4f8bbf_f1f2_418a_b35e_a195bc4103b9
);
windows::core::imp::interface_hierarchy!(AccessibleHypertext, IUnknown, AccessibleText);

#[repr(C)]
pub struct AccessibleText2Vtable {
    pub base: AccessibleTextVtable,
    pub attributeRange: unsafe extern "system" fn(
        *mut c_void,
        i32,
        *const u16,
        *mut i32,
        *mut i32,
        *mut BSTR,
    ) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleText2,
    AccessibleText2Vtable,
    0x9690a9cc5c804df5852e2d5ae4189a54
);
windows::core::imp::interface_hierarchy!(AccessibleText2, IUnknown, AccessibleText);

#[repr(C)]
pub struct AccessibleTable2Vtable {
    pub base: IUnknown_Vtbl,
    pub cellAt: unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> HRESULT,
    pub caption: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub columnDescription: unsafe extern "system" fn(*mut c_void, i32, *mut BSTR) -> HRESULT,
    pub nColumns: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub nRows: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub nSelectedCells: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub nSelectedColumns: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub nSelectedRows: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub rowDescription: unsafe extern "system" fn(*mut c_void, i32, *mut BSTR) -> HRESULT,
    pub selectedCells:
        unsafe extern "system" fn(*mut c_void, *mut *mut *mut c_void, *mut i32) -> HRESULT,
    pub selectedColumns: unsafe extern "system" fn(*mut c_void, *mut *mut i32, *mut i32) -> HRESULT,
    pub selectedRows: unsafe extern "system" fn(*mut c_void, *mut *mut i32, *mut i32) -> HRESULT,
    pub summary: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub isColumnSelected: unsafe extern "system" fn(*mut c_void, i32, *mut u8) -> HRESULT,
    pub isRowSelected: unsafe extern "system" fn(*mut c_void, i32, *mut u8) -> HRESULT,
    pub selectRow: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub selectColumn: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub unselectRow: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub unselectColumn: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub modelChange: unsafe extern "system" fn(*mut c_void, *mut TableChange) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleTable2,
    AccessibleTable2Vtable,
    0x6167f29506f04cdda1fa02e25153d869
);
windows::core::imp::interface_hierarchy!(AccessibleTable2, IUnknown);

#[repr(C)]
pub struct AccessibleTableCellVtable {
    pub base: IUnknown_Vtbl,
    pub columnExtent: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub columnHeaderCells:
        unsafe extern "system" fn(*mut c_void, *mut *mut *mut c_void, *mut i32) -> HRESULT,
    pub columnIndex: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub rowExtent: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub rowHeaderCells:
        unsafe extern "system" fn(*mut c_void, *mut *mut *mut c_void, *mut i32) -> HRESULT,
    pub rowIndex: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub isSelected: unsafe extern "system" fn(*mut c_void, *mut u8) -> HRESULT,
    pub rowColumnExtents: unsafe extern "system" fn(
        *mut c_void,
        *mut i32,
        *mut i32,
        *mut i32,
        *mut i32,
        *mut u8,
    ) -> HRESULT,
    pub table: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleTableCell,
    AccessibleTableCellVtable,
    0x594116b1c99f4847ad060a7a86ece645
);
windows::core::imp::interface_hierarchy!(AccessibleTableCell, IUnknown);

#[repr(C)]
pub struct AccessibleActionVtable {
    pub base: IUnknown_Vtbl,
    pub nActions: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    pub doAction: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    pub description: unsafe extern "system" fn(*mut c_void, i32, *mut BSTR) -> HRESULT,
    pub keyBinding:
        unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut BSTR, *mut i32) -> HRESULT,
    pub name: unsafe extern "system" fn(*mut c_void, i32, *mut BSTR) -> HRESULT,
    pub localizedName: unsafe extern "system" fn(*mut c_void, i32, *mut BSTR) -> HRESULT,
}
windows::core::imp::define_interface!(
    AccessibleAction,
    AccessibleActionVtable,
    0xb70d9f593b5a4dbaab9e22012f607df5
);
windows::core::imp::interface_hierarchy!(AccessibleAction, IUnknown);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_slots_keep_inherited_prefixes_and_idl_order() {
        let slot = std::mem::size_of::<usize>();
        let unknown = std::mem::size_of::<IUnknown_Vtbl>();
        assert_eq!(
            std::mem::offset_of!(AccessibleTextVtable, text),
            unknown + 7 * slot
        );
        assert_eq!(
            std::mem::offset_of!(AccessibleTextVtable, nCharacters),
            unknown + 14 * slot
        );
        assert_eq!(
            std::mem::offset_of!(AccessibleText2Vtable, attributeRange),
            std::mem::size_of::<AccessibleTextVtable>()
        );
        assert_eq!(
            std::mem::offset_of!(AccessibleTable2Vtable, selectedCells),
            unknown + 9 * slot
        );
        assert_eq!(
            std::mem::offset_of!(AccessibleTableCellVtable, rowColumnExtents),
            unknown + 7 * slot
        );
        assert_eq!(
            std::mem::size_of::<AccessibleTableCellVtable>(),
            unknown + 9 * slot
        );
    }
}
