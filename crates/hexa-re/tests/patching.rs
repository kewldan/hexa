//! RE-03 / RE-05 / RE-06 workflow on a hand-built PE.

mod common;

use hexa_core::Tree;
use hexa_core::piece::Piece;
use hexa_re::Arch;
use hexa_re::checksum::image_checksum;
use hexa_re::disasm::{DisasmOptions, disassemble_tree};
use hexa_re::exe::parse_tree;
use hexa_re::patch::{
    PatchList, PatchState, PortOptions, PortStatus, plan_instruction_patch_tree, port_patches,
};

#[test]
fn patch_entry_point_and_port() {
    let data = common::pe64();
    let tree = Tree::from_piece(Piece::bytes(data.clone()));
    let info = parse_tree(&tree).unwrap();
    let entry = info.entry_point.unwrap();
    let off = info.entry_offset.unwrap();

    // Replace `sub rsp, 0x28` (4 bytes) by `xor eax, eax; ret` (3 bytes).
    let plan =
        plan_instruction_patch_tree(Arch::X64, &tree, off, entry, "xor eax, eax; ret").unwrap();
    assert_eq!(plan.assembled_len, 3);
    assert_eq!(plan.original_instruction_len, 4);
    assert_eq!(plan.replacement, vec![0x31, 0xc0, 0xc3, 0x90]);
    assert!(plan.warning.is_none());

    // A longer patch overwrites the following `lea`.
    let long =
        plan_instruction_patch_tree(Arch::X64, &tree, off, entry, "mov rax, 0x1122334455667788")
            .unwrap();
    assert_eq!(long.extra_overwritten, 1);
    assert_eq!(long.padding, 1);

    let mut list = PatchList::new(vec![plan.to_patch(off, "return 0")]);
    list.target = Some("test.dll".into());
    let json = list.to_json().unwrap();
    let list = PatchList::from_json(&json).unwrap();
    let patched = list.apply_to_tree(&tree, false).unwrap();
    assert_eq!(
        list.states_tree(&patched).unwrap(),
        vec![PatchState::Applied]
    );
    let ins = disassemble_tree(
        Arch::X64,
        &patched,
        off,
        4,
        entry,
        &DisasmOptions::default(),
    )
    .unwrap();
    let text: Vec<String> = ins.iter().map(hexa_re::disasm::Instruction::text).collect();
    assert_eq!(text, vec!["xor eax, eax", "ret", "nop"]);

    // The checksum must be refreshed after patching.
    let bytes = hexa_re::tree_bytes(&patched).unwrap();
    assert!(!image_checksum(&bytes).is_valid());

    // Port the patch to a "new version" with 0x40 bytes of overlay
    // inserted before the code section.
    let mut newer = data[..0x300].to_vec();
    newer.extend(vec![0xccu8; 0x40]);
    newer.extend(&data[0x300..]);
    let report = port_patches(&data, &newer, &list, &PortOptions::default());
    assert_eq!(report.results[0].status, PortStatus::Ported);
    assert_eq!(report.results[0].new_offset, Some(off + 0x40));
    assert_eq!(report.ported.patches[0].offset, off + 0x40);
}
