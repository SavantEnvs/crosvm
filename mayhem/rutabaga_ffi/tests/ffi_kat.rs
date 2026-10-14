// Known-answer tests for rutabaga_gfx's C ABI over its 2D component: the create_3d -> attach_backing
// -> transfer_write -> transfer_read pixel path and the error codes of the blob/unref calls that
// rutabaga_ffi_fuzzer drives. Run by mayhem/test.sh. Only well-formed calls: a NULL iovec array with
// zero entries (create_blob) is the fuzzer's business, not a test of the honest contract.
use std::os::raw::c_void;
use std::ptr::null;
use std::ptr::null_mut;

use libc::iovec;
use libc::EINVAL;
use rutabaga_ffi::ffi::*;
use rutabaga_gfx::ResourceCreate3D;
use rutabaga_gfx::ResourceCreateBlob;
use rutabaga_gfx::Rutabaga;
use rutabaga_gfx::RutabagaFence;
use rutabaga_gfx::Transfer3D;

extern "C" fn fence_cb(_user_data: u64, _fence: &RutabagaFence) {}

fn new_rutabaga() -> *mut Rutabaga {
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
    assert_eq!(unsafe { rutabaga_init(&builder, &mut ptr) }, 0);
    assert!(!ptr.is_null());
    ptr
}

fn create_4x4(r: &mut Rutabaga, id: u32) {
    let create = ResourceCreate3D {
        target: 2,
        format: 1,
        bind: 0,
        width: 4,
        height: 4,
        depth: 1,
        array_size: 1,
        last_level: 0,
        nr_samples: 0,
        flags: 0,
    };
    assert_eq!(rutabaga_resource_create_3d(r, id, &create), 0);
}

fn rect(x: u32, y: u32, w: u32, h: u32, stride: u32) -> Transfer3D {
    Transfer3D {
        x,
        y,
        z: 0,
        w,
        h,
        d: 1,
        level: 0,
        stride,
        layer_stride: 0,
        offset: 0,
    }
}

#[test]
fn pixels_round_trip_through_two_iovecs() {
    let mut rtbg = new_rutabaga();
    let r = unsafe { &mut *rtbg };
    create_4x4(r, 1);
    // 4x4 pixels at 4 bytes each = 64 bytes of guest memory, split 40 + 24 across two iovecs.
    let pattern: Vec<u8> = (0..64u8).collect();
    let mut a = pattern[..40].to_vec();
    let mut b = pattern[40..].to_vec();
    let mut iovs = [
        iovec { iov_base: a.as_mut_ptr() as *mut c_void, iov_len: a.len() },
        iovec { iov_base: b.as_mut_ptr() as *mut c_void, iov_len: b.len() },
    ];
    let backing = rutabaga_iovecs { iovecs: iovs.as_mut_ptr(), num_iovecs: 2 };
    assert_eq!(unsafe { rutabaga_resource_attach_backing(r, 1, &backing) }, 0);
    assert_eq!(rutabaga_resource_transfer_write(r, 0, 1, &rect(0, 0, 4, 4, 0)), 0);

    // The whole image comes back byte for byte.
    let mut out = vec![0u8; 64];
    let dst = iovec { iov_base: out.as_mut_ptr() as *mut c_void, iov_len: out.len() };
    assert_eq!(unsafe { rutabaga_resource_transfer_read(r, 0, 1, &rect(0, 0, 4, 4, 16), Some(&dst)) }, 0);
    assert_eq!(out, pattern);

    // The 2x2 rectangle at (1,1): pixels 5,6 of row 1 and 9,10 of row 2, written with a 16 byte stride.
    let mut out = vec![0xEEu8; 64];
    let dst = iovec { iov_base: out.as_mut_ptr() as *mut c_void, iov_len: out.len() };
    assert_eq!(unsafe { rutabaga_resource_transfer_read(r, 0, 1, &rect(1, 1, 2, 2, 16), Some(&dst)) }, 0);
    let row1: Vec<u8> = (20..28).collect();
    let row2: Vec<u8> = (36..44).collect();
    assert_eq!(&out[20..28], &row1[..]);
    assert_eq!(&out[36..44], &row2[..]);
    assert!(out[..20].iter().all(|&v| v == 0xEE));
    assert!(out[28..36].iter().all(|&v| v == 0xEE));
    assert!(out[44..].iter().all(|&v| v == 0xEE));

    assert_eq!(rutabaga_resource_unref(r, 1), 0);
    assert_eq!(rutabaga_finish(&mut rtbg), 0);
    assert!(rtbg.is_null());
}

#[test]
fn out_of_range_rectangle_and_unknown_resource_are_rejected() {
    let mut rtbg = new_rutabaga();
    let r = unsafe { &mut *rtbg };
    create_4x4(r, 7);
    let mut out = vec![0u8; 64];
    let dst = iovec { iov_base: out.as_mut_ptr() as *mut c_void, iov_len: out.len() };
    // x + w = 5 > 4 pixels wide.
    assert_eq!(
        unsafe { rutabaga_resource_transfer_read(r, 0, 7, &rect(2, 0, 3, 1, 16), Some(&dst)) },
        -EINVAL
    );
    // No such resource.
    assert_eq!(
        unsafe { rutabaga_resource_transfer_read(r, 0, 8, &rect(0, 0, 1, 1, 16), Some(&dst)) },
        -EINVAL
    );
    // Creating the same id twice, and unref of an id that is gone, fail.
    let create = ResourceCreate3D {
        target: 2, format: 1, bind: 0, width: 1, height: 1, depth: 1,
        array_size: 1, last_level: 0, nr_samples: 0, flags: 0,
    };
    assert_eq!(rutabaga_resource_create_3d(r, 7, &create), -EINVAL);
    assert_eq!(rutabaga_resource_unref(r, 7), 0);
    assert_eq!(rutabaga_resource_unref(r, 7), -EINVAL);
    assert_eq!(rutabaga_finish(&mut rtbg), 0);
}

#[test]
fn blob_creation_is_unsupported_by_the_2d_component() {
    let mut rtbg = new_rutabaga();
    let r = unsafe { &mut *rtbg };
    let blob = ResourceCreateBlob { blob_mem: 1, blob_flags: 0, blob_id: 0, size: 4096 };
    let mut buf = vec![0u8; 4096];
    let mut iovs = [iovec { iov_base: buf.as_mut_ptr() as *mut c_void, iov_len: buf.len() }];
    let backing = rutabaga_iovecs { iovecs: iovs.as_mut_ptr(), num_iovecs: 1 };
    assert_eq!(unsafe { rutabaga_resource_create_blob(r, 0, 3, &blob, Some(&backing), None) }, -EINVAL);
    assert_eq!(unsafe { rutabaga_resource_create_blob(r, 0, 3, &blob, None, None) }, -EINVAL);
    // A failed create leaves no resource behind: the id is still free for create_3d.
    create_4x4(r, 3);
    assert_eq!(rutabaga_resource_unref(r, 3), 0);
    assert_eq!(rutabaga_finish(&mut rtbg), 0);
}
