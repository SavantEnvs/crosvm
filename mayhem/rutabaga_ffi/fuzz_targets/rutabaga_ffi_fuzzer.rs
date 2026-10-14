// Drives rutabaga_gfx's C ABI (rutabaga_init / create_3d / attach_backing / transfer_write /
// transfer_read / create_blob / unref) the way a C embedder would, over one 2D rutabaga instance.
// Input: a stream of ops, each `tag:u8` then its fields (little endian, zero-padded at the end).
//   tag % 5 == 0  create_3d      id:u8 w:u8 h:u8
//   tag % 5 == 1  attach_backing id:u8 n:u8 (n % 4 + 1 iovecs: len:u8 -> len*4 bytes of input)
//   tag % 5 == 2  transfer       id:u8 write:u8 x,y,w,h:u8 stride:u8 offset:u8 out_len:u8
//   tag % 5 == 3  create_blob    id:u8 ctx:u8 mem:u8 flags:u8 blob_id:u64 size:u32 iov:u8
//                                  iov % 4: 0 no iovecs struct, 1 {NULL, 0}, 2 {non-null, 0}, 3 n real iovecs
//   tag % 5 == 4  unref          id:u8
#![no_main]

use std::cell::Cell;
use std::os::raw::c_void;
use std::ptr::null;
use std::ptr::null_mut;

use libfuzzer_sys::fuzz_target;
use rutabaga_ffi::ffi::*;
use rutabaga_gfx::ResourceCreate3D;
use rutabaga_gfx::ResourceCreateBlob;
use rutabaga_gfx::Rutabaga;
use rutabaga_gfx::RutabagaFence;
use rutabaga_gfx::Transfer3D;

extern "C" fn fence_cb(_user_data: u64, _fence: &RutabagaFence) {}

thread_local! {
    static RUTABAGA: Cell<*mut Rutabaga> = const { Cell::new(null_mut()) };
}

fn instance() -> &'static mut Rutabaga {
    RUTABAGA.with(|cell| {
        if cell.get().is_null() {
            let builder = rutabaga_builder {
                user_data: 0,
                capset_mask: 0,
                wsi: 1,
                fence_cb,
                debug_cb: None,
                channels: None,
                renderer_features: null(),
            };
            let mut ptr: *mut Rutabaga = null_mut();
            let rc = unsafe { rutabaga_init(&builder, &mut ptr) };
            assert_eq!(rc, 0, "rutabaga_init failed");
            cell.set(ptr);
        }
        unsafe { &mut *cell.get() }
    })
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> u8 {
        let v = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        v
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes([self.u8(), self.u8(), self.u8(), self.u8()])
    }
    fn u64(&mut self) -> u64 {
        (self.u32() as u64) | ((self.u32() as u64) << 32)
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let end = (self.pos + len).min(self.data.len());
        let mut v = if self.pos < end {
            self.data[self.pos..end].to_vec()
        } else {
            Vec::new()
        };
        v.resize(len, 0);
        self.pos += len;
        v
    }
    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }
}

fn read_iovecs(r: &mut Reader, buffers: &mut Vec<Vec<u8>>) -> Vec<libc::iovec> {
    let n = r.u8() as usize % 4 + 1;
    let mut iovs = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.u8() as usize * 4;
        buffers.push(r.bytes(len));
        let b = buffers.last_mut().unwrap();
        iovs.push(libc::iovec {
            iov_base: b.as_mut_ptr() as *mut c_void,
            iov_len: b.len(),
        });
    }
    iovs
}

fuzz_target!(|data: &[u8]| {
    let rtbg = instance();
    let mut r = Reader { data, pos: 0 };
    // Backing memory and iovec arrays must outlive the resources that were handed them.
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    let mut iovec_arrays: Vec<Vec<libc::iovec>> = Vec::new();
    let mut ops = 0;

    while !r.done() && ops < 32 {
        ops += 1;
        let tag = r.u8();
        let id = r.u8() as u32 % 16;
        match tag % 5 {
            0 => {
                let create = ResourceCreate3D {
                    target: 2,
                    format: 1,
                    bind: 0,
                    width: r.u8() as u32 % 33,
                    height: r.u8() as u32 % 33,
                    depth: 1,
                    array_size: 1,
                    last_level: 0,
                    nr_samples: 0,
                    flags: 0,
                };
                rutabaga_resource_create_3d(rtbg, id, &create);
            }
            1 => {
                let iovs = read_iovecs(&mut r, &mut buffers);
                iovec_arrays.push(iovs);
                let arr = iovec_arrays.last_mut().unwrap();
                let rutabaga_iovs = rutabaga_iovecs {
                    iovecs: arr.as_mut_ptr(),
                    num_iovecs: arr.len(),
                };
                unsafe { rutabaga_resource_attach_backing(rtbg, id, &rutabaga_iovs) };
            }
            2 => {
                let write = r.u8() & 1 == 1;
                let transfer = Transfer3D {
                    x: r.u8() as u32,
                    y: r.u8() as u32,
                    z: 0,
                    w: r.u8() as u32,
                    h: r.u8() as u32,
                    d: 1,
                    level: 0,
                    stride: r.u8() as u32 * 4,
                    layer_stride: 0,
                    offset: r.u8() as u64 * 4,
                };
                let out_len = r.u8() as usize * 16;
                if write {
                    rutabaga_resource_transfer_write(rtbg, 0, id, &transfer);
                } else {
                    let mut out = vec![0u8; out_len];
                    let iov = libc::iovec {
                        iov_base: out.as_mut_ptr() as *mut c_void,
                        iov_len: out.len(),
                    };
                    unsafe { rutabaga_resource_transfer_read(rtbg, 0, id, &transfer, Some(&iov)) };
                }
            }
            3 => {
                let ctx_id = r.u8() as u32 % 2;
                let blob = ResourceCreateBlob {
                    blob_mem: r.u8() as u32 % 4,
                    blob_flags: r.u8() as u32 % 8,
                    blob_id: r.u64(),
                    size: r.u32() as u64,
                };
                let null_iovs = rutabaga_iovecs {
                    iovecs: null_mut(),
                    num_iovecs: 0,
                };
                let mut empty: Vec<libc::iovec> = Vec::new();
                let nonnull_empty = rutabaga_iovecs {
                    iovecs: empty.as_mut_ptr(),
                    num_iovecs: 0,
                };
                let mode = r.u8() % 4;
                let real_iovs;
                let real;
                let iovs: Option<&rutabaga_iovecs> = match mode {
                    0 => None,
                    1 => Some(&null_iovs),
                    2 => Some(&nonnull_empty),
                    _ => {
                        let mut v = read_iovecs(&mut r, &mut buffers);
                        real_iovs = rutabaga_iovecs {
                            iovecs: v.as_mut_ptr(),
                            num_iovecs: v.len(),
                        };
                        iovec_arrays.push(v);
                        real = &real_iovs;
                        Some(real)
                    }
                };
                unsafe { rutabaga_resource_create_blob(rtbg, ctx_id, id, &blob, iovs, None) };
            }
            _ => {
                rutabaga_resource_unref(rtbg, id);
            }
        }
    }

    for id in 0..16 {
        rutabaga_resource_unref(rtbg, id);
    }
});
