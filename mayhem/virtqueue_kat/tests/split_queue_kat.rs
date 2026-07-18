// Known-answer tests for crosvm's split virtqueue (virtio 1.x "split" layout), driven through the
// public devices::virtio::Queue API: QueueConfig::activate -> Queue::pop / Queue::peek ->
// DescriptorChain reader/writer -> Queue::add_used*. This is the path virtqueue_fuzzer exercises.
//
// Every ring here is WELL-FORMED and spec-aligned (virtio 1.x 2.7: descriptor table 16-byte, avail
// ring 2-byte, used ring 4-byte aligned). Most tests use page-aligned rings; the min_aligned_* tests
// place each ring at exactly its minimum alignment and no more (0x1010 / 0x2002 / 0x3004), which a
// driver may legally do, so a "fix" that demands more alignment than the spec (e.g. page-aligned
// rings only) breaks real devices and fails them. The tests assert only what the virtio spec defines
// for such rings, so they do not depend on how malformed or misaligned rings are handled: rejecting
// them at activation and reading them unaligned both pass.
use std::io::Read;
use std::io::Write;

use base::Event;
use devices::virtio::Interrupt;
use devices::virtio::Queue;
use devices::virtio::QueueConfig;
use devices::IrqLevelEvent;
use vm_memory::GuestAddress;
use vm_memory::GuestMemory;

const MEM_SIZE: u64 = 0x10000;
const DESC: u64 = 0x1000;
const AVAIL: u64 = 0x2000;
const USED: u64 = 0x3000;
const DATA: u64 = 0x4000;

// Guest-physical placement of the three rings of one split queue.
#[derive(Clone, Copy)]
struct Rings {
    desc: u64,
    avail: u64,
    used: u64,
}

const PAGE_ALIGNED: Rings = Rings {
    desc: DESC,
    avail: AVAIL,
    used: USED,
};

// Each ring at exactly the alignment virtio 1.x 2.7 requires, and not one bit more: 0x1010 is
// 16- but not 32-byte aligned, 0x2002 is 2- but not 4-byte aligned, 0x3004 is 4- but not 8-byte
// aligned. For every queue size used below the three rings stay disjoint and below DATA.
const MIN_ALIGNED: Rings = Rings {
    desc: DESC + 0x10,
    avail: AVAIL + 0x2,
    used: USED + 0x4,
};
const F_NEXT: u16 = 1;
const F_WRITE: u16 = 2;

fn setup(size: u16) -> (GuestMemory, Queue) {
    setup_at(size, PAGE_ALIGNED)
}

fn setup_at(size: u16, rings: Rings) -> (GuestMemory, Queue) {
    let mem = GuestMemory::new(&[(GuestAddress(0), MEM_SIZE)]).unwrap();
    let mut cfg = QueueConfig::new(size, 0);
    cfg.set_size(size);
    cfg.set_desc_table(GuestAddress(rings.desc));
    cfg.set_avail_ring(GuestAddress(rings.avail));
    cfg.set_used_ring(GuestAddress(rings.used));
    cfg.set_ready(true);
    let interrupt = Interrupt::new(
        IrqLevelEvent::new().unwrap(),
        None,
        0xFFFF,
        #[cfg(target_arch = "x86_64")]
        None,
    );
    let q = cfg
        .activate(&mem, Event::new().unwrap(), interrupt)
        .expect("a well-formed, aligned split queue must activate");
    (mem, q)
}

fn w16(mem: &GuestMemory, a: u64, v: u16) {
    mem.write_all_at_addr(&v.to_le_bytes(), GuestAddress(a)).unwrap();
}

fn r16(mem: &GuestMemory, a: u64) -> u16 {
    let mut b = [0u8; 2];
    mem.read_exact_at_addr(&mut b, GuestAddress(a)).unwrap();
    u16::from_le_bytes(b)
}

fn r32(mem: &GuestMemory, a: u64) -> u32 {
    let mut b = [0u8; 4];
    mem.read_exact_at_addr(&mut b, GuestAddress(a)).unwrap();
    u32::from_le_bytes(b)
}

// Write descriptor `i` of the descriptor table (struct virtq_desc: addr, len, flags, next).
fn desc(mem: &GuestMemory, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
    desc_at(mem, PAGE_ALIGNED, i, addr, len, flags, next)
}

fn desc_at(mem: &GuestMemory, rings: Rings, i: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&addr.to_le_bytes());
    b[8..12].copy_from_slice(&len.to_le_bytes());
    b[12..14].copy_from_slice(&flags.to_le_bytes());
    b[14..16].copy_from_slice(&next.to_le_bytes());
    mem.write_all_at_addr(&b, GuestAddress(rings.desc + 16 * i as u64))
        .unwrap();
}

// Publish `head` at avail ring slot `slot` (already reduced mod size by the caller) and set
// avail.idx to `new_idx`.
fn publish(mem: &GuestMemory, slot: u16, head: u16, new_idx: u16) {
    publish_at(mem, PAGE_ALIGNED, slot, head, new_idx)
}

fn publish_at(mem: &GuestMemory, rings: Rings, slot: u16, head: u16, new_idx: u16) {
    w16(mem, rings.avail + 4 + 2 * slot as u64, head);
    w16(mem, rings.avail + 2, new_idx);
}

fn used_idx(mem: &GuestMemory) -> u16 {
    used_idx_at(mem, PAGE_ALIGNED)
}

fn used_idx_at(mem: &GuestMemory, rings: Rings) -> u16 {
    r16(mem, rings.used + 2)
}

// (id, len) of used ring element `slot`.
fn used_elem(mem: &GuestMemory, slot: u64) -> (u32, u32) {
    used_elem_at(mem, PAGE_ALIGNED, slot)
}

fn used_elem_at(mem: &GuestMemory, rings: Rings, slot: u64) -> (u32, u32) {
    (
        r32(mem, rings.used + 4 + 8 * slot),
        r32(mem, rings.used + 8 + 8 * slot),
    )
}

#[test]
fn pop_single_readable_descriptor() {
    let (mem, mut q) = setup(16);
    mem.write_all_at_addr(b"crosvm!!", GuestAddress(DATA)).unwrap();
    desc(&mem, 0, DATA, 8, 0, 0);
    publish(&mem, 0, 0, 1);
    let mut c = q.pop().expect("a published chain must be popped");
    assert_eq!(c.index(), 0);
    assert_eq!(c.reader.available_bytes(), 8);
    assert_eq!(c.writer.available_bytes(), 0);
    let mut buf = [0u8; 8];
    c.reader.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"crosvm!!");
    assert!(q.pop().is_none(), "ring holds exactly one chain");
}

#[test]
fn pop_chain_readable_then_writable() {
    let (mem, mut q) = setup(16);
    desc(&mem, 0, DATA, 4, F_NEXT, 1);
    desc(&mem, 1, DATA + 0x100, 12, F_WRITE, 0);
    publish(&mem, 0, 0, 1);
    let mut c = q.pop().expect("chain");
    assert_eq!(c.reader.available_bytes(), 4);
    assert_eq!(c.writer.available_bytes(), 12);
    c.writer.write_all(b"hello world!").unwrap();
    let mut out = [0u8; 12];
    mem.read_exact_at_addr(&mut out, GuestAddress(DATA + 0x100))
        .unwrap();
    assert_eq!(&out, b"hello world!");
}

#[test]
fn add_used_publishes_head_and_bytes_written() {
    let (mem, mut q) = setup(16);
    desc(&mem, 5, DATA, 32, F_WRITE, 0);
    publish(&mem, 0, 5, 1);
    let mut c = q.pop().expect("chain");
    c.writer.write_all(&[0xAB; 10]).unwrap();
    q.add_used(c);
    assert_eq!(used_idx(&mem), 1);
    assert_eq!(used_elem(&mem, 0), (5, 10));
}

#[test]
fn add_used_with_explicit_length() {
    let (mem, mut q) = setup(8);
    desc(&mem, 3, DATA, 64, F_WRITE, 0);
    publish(&mem, 0, 3, 1);
    let c = q.pop().expect("chain");
    q.add_used_with_bytes_written(c, 7);
    assert_eq!(used_idx(&mem), 1);
    assert_eq!(used_elem(&mem, 0), (3, 7));
}

#[test]
fn pops_follow_avail_ring_order() {
    let (mem, mut q) = setup(8);
    for i in 0..4u16 {
        desc(&mem, i, DATA + 0x40 * i as u64, (i + 1) as u32, 0, 0);
    }
    for (slot, head) in [2u16, 0, 3, 1].iter().enumerate() {
        w16(&mem, AVAIL + 4 + 2 * slot as u64, *head);
    }
    w16(&mem, AVAIL + 2, 4);
    let mut got = vec![];
    while let Some(c) = q.pop() {
        got.push((c.index(), c.reader.available_bytes()));
    }
    assert_eq!(got, vec![(2, 3), (0, 1), (3, 4), (1, 2)]);
}

#[test]
fn avail_ring_wraps_modulo_queue_size() {
    let (mem, mut q) = setup(4);
    for i in 0..4u16 {
        desc(&mem, i, DATA + 0x40 * i as u64, 16, F_WRITE, 0);
        publish(&mem, i, i, i + 1);
    }
    for want in 0..4u16 {
        let c = q.pop().expect("round 1");
        assert_eq!(c.index(), want);
        q.add_used_with_bytes_written(c, 1);
    }
    assert!(q.pop().is_none());
    // Round 2: avail idx 4 and 5 live in ring slots 0 and 1.
    publish(&mem, 0, 3, 5);
    publish(&mem, 1, 2, 6);
    let mut heads = vec![];
    while let Some(c) = q.pop() {
        heads.push(c.index());
    }
    assert_eq!(heads, vec![3, 2]);
    assert_eq!(used_idx(&mem), 4);
}

#[test]
fn peek_does_not_consume() {
    let (mem, mut q) = setup(16);
    desc(&mem, 9, DATA, 8, 0, 0);
    publish(&mem, 0, 9, 1);
    {
        let p = q.peek().expect("peek must see the published chain");
        assert_eq!(p.index(), 9);
    }
    let c = q.pop().expect("chain still available after peek");
    assert_eq!(c.index(), 9);
    assert!(q.pop().is_none());
}

#[test]
fn empty_avail_ring_yields_nothing() {
    let (_mem, mut q) = setup(16);
    assert!(q.pop().is_none());
}

// A queue whose rings sit at exactly the spec's minimum alignments must activate, and a published
// readable -> writable chain must pop with the right bytes, then land in the used ring at the
// driver's (not a rounded) address.
#[test]
fn min_aligned_rings_pop_chain_and_publish_used() {
    let r = MIN_ALIGNED;
    let (mem, mut q) = setup_at(16, r);
    mem.write_all_at_addr(b"virtio!!", GuestAddress(DATA)).unwrap();
    desc_at(&mem, r, 0, DATA, 8, F_NEXT, 1);
    desc_at(&mem, r, 1, DATA + 0x100, 16, F_WRITE, 0);
    publish_at(&mem, r, 0, 0, 1);
    let mut c = q
        .pop()
        .expect("a chain published on minimally aligned rings must be popped");
    assert_eq!(c.index(), 0);
    assert_eq!(c.reader.available_bytes(), 8);
    assert_eq!(c.writer.available_bytes(), 16);
    let mut buf = [0u8; 8];
    c.reader.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"virtio!!");
    c.writer.write_all(b"used!").unwrap();
    q.add_used(c);
    assert!(q.pop().is_none(), "ring holds exactly one chain");
    let mut out = [0u8; 5];
    mem.read_exact_at_addr(&mut out, GuestAddress(DATA + 0x100))
        .unwrap();
    assert_eq!(&out, b"used!");
    assert_eq!(used_idx_at(&mem, r), 1);
    assert_eq!(used_elem_at(&mem, r, 0), (0, 5));
    // Nothing went to the page-aligned addresses the rings were deliberately moved off.
    assert_eq!(r32(&mem, USED), 0, "used ring header written at a rounded-down address");
}

// Several chains through a wrapping avail ring on minimally aligned rings: every head comes out in
// avail order, peek does not consume, and every used element lands at its own slot.
#[test]
fn min_aligned_rings_wrap_in_order() {
    let r = MIN_ALIGNED;
    let (mem, mut q) = setup_at(4, r);
    for i in 0..4u16 {
        desc_at(&mem, r, i, DATA + 0x40 * i as u64, 0x20, F_WRITE, 0);
        publish_at(&mem, r, i, 3 - i, i + 1);
    }
    assert_eq!(q.peek().expect("peek on minimally aligned rings").index(), 3);
    for (n, want) in [3u16, 2, 1, 0].iter().enumerate() {
        let c = q.pop().expect("round 1");
        assert_eq!(c.index(), *want);
        assert_eq!(c.writer.available_bytes(), 0x20);
        q.add_used_with_bytes_written(c, 0x10 + n as u32);
    }
    assert!(q.pop().is_none());
    assert_eq!(used_idx_at(&mem, r), 4);
    for n in 0..4u64 {
        assert_eq!(used_elem_at(&mem, r, n), (3 - n as u32, 0x10 + n as u32));
    }
    // Round 2: avail idx 4 and 5 live in ring slots 0 and 1.
    publish_at(&mem, r, 0, 1, 5);
    publish_at(&mem, r, 1, 2, 6);
    let mut heads = vec![];
    while let Some(c) = q.pop() {
        heads.push(c.index());
        q.add_used_with_bytes_written(c, 7);
    }
    assert_eq!(heads, vec![1, 2]);
    assert_eq!(used_idx_at(&mem, r), 6);
    assert_eq!(used_elem_at(&mem, r, 0), (1, 7));
    assert_eq!(used_elem_at(&mem, r, 1), (2, 7));
}
