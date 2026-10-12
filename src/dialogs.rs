//! The few stock Windows dialogs the app uses.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Shell::*;

use crate::util::{from_wide, pcwstr, take_pwstr, wide};

pub fn save_as(owner: HWND, dir: &Path, name: &str) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    let mut file = [0u16; 1024];
    let initial: Vec<u16> = format!("{name}.png").encode_utf16().collect();
    file[..initial.len()].copy_from_slice(&initial);
    let filter: Vec<u16> = "PNG image (*.png)\0*.png\0JPEG image (*.jpg)\0*.jpg;*.jpeg\0\0".encode_utf16().collect();
    let dir_w = wide(&dir.to_string_lossy());
    let ext = wide("png");
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: pcwstr(&filter),
        lpstrFile: windows::core::PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrInitialDir: pcwstr(&dir_w),
        lpstrDefExt: pcwstr(&ext),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
        ..Default::default()
    };
    unsafe {
        if !GetSaveFileNameW(&mut ofn).as_bool() {
            return None;
        }
    }
    let mut path = PathBuf::from(from_wide(&file));
    // Picking the JPEG filter but typing a bare name should still give a .jpg.
    if ofn.nFilterIndex == 2 && path.extension().is_none() {
        path.set_extension("jpg");
    }
    Some(path)
}

/// `rgb` is 0xRRGGBB in and out.
pub fn choose_color(owner: HWND, rgb: u32) -> Option<u32> {
    thread_local! {
        static CUSTOM: std::cell::RefCell<[COLORREF; 16]> = const { std::cell::RefCell::new([COLORREF(0xffffff); 16]) };
    }
    let to_ref = |c: u32| COLORREF(((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff));
    CUSTOM.with(|custom| {
        let mut custom = custom.borrow_mut();
        let mut cc = CHOOSECOLORW {
            lStructSize: size_of::<CHOOSECOLORW>() as u32,
            hwndOwner: owner,
            rgbResult: to_ref(rgb),
            lpCustColors: custom.as_mut_ptr(),
            Flags: CC_RGBINIT | CC_FULLOPEN,
            ..Default::default()
        };
        unsafe {
            if !ChooseColorW(&mut cc).as_bool() {
                return None;
            }
        }
        let c = cc.rgbResult.0;
        Some(((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff))
    })
}

pub fn pick_folder(owner: HWND, initial: &Path) -> Option<String> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let init = wide(&initial.to_string_lossy());
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(pcwstr(&init), None) {
            let _ = dlg.SetFolder(&item);
        }
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let path = take_pwstr(item.GetDisplayName(SIGDN_FILESYSPATH).ok()?);
        Some(path)
    }
}
