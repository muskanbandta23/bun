use crate::assembler::Assembler;
use crate::*;

fn local(index: u32) -> Register {
    Register::local(index)
}

fn listing(body: &Body) -> Vec<(u32, &'static str, Vec<i32>)> {
    let mut offset = 0;
    let mut listing = vec![];
    while let Some(instruction) = RawInstruction::read(body.instructions(), offset) {
        let words = instruction.words().iter().map(|&word| word as i32);
        listing.push((offset, instruction.opcode.mnemonic(), words.collect()));
        offset += u32::from(instruction.size);
    }
    assert_eq!(offset as usize, body.instructions().len());
    listing
}

fn finish(assembler: &mut Assembler) -> Body {
    assembler
        .finish(CodeKind::Call, CodeFlags::empty(), 16, 1)
        .unwrap()
}

#[test]
fn branches() {
    let mut a = Assembler::default();
    let (otherwise, end, out) = (a.label(), a.label(), a.label());
    a.enter(0);
    a.jump_if_false(local(1), otherwise);
    a.mov(local(2), Operand::constant(0));
    a.jump(end);
    a.bind(otherwise);
    a.jump(out);
    a.bind(end);
    a.mov(local(3), local(2));
    a.bind(out);
    a.ret(local(3));
    let k0 = Operand::constant(0).word() as i32;
    assert_eq!(
        listing(&finish(&mut a)),
        [
            (0, "enter", vec![0]),
            (2, "jump_if_false", vec![-2, 9]),
            (5, "mov", vec![-3, k0]),
            (8, "mov", vec![-4, -3]),
            (11, "ret", vec![-4]),
        ]
    );
}
