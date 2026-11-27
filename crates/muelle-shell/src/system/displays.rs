//! Monitor identities and names from the active Windows display paths.

use windows::Win32::{
    Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    },
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, POINT},
};

use super::{Monitor, monitor_at};

#[derive(serde::Serialize)]
pub struct DesktopDisplay {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    origin: POINT,
}

/// The saved target is optional and may be unplugged. The caller supplies
/// the already queried primary monitor as the fallback.
#[must_use]
pub fn selected_monitor(id: Option<&str>, primary: Monitor) -> Monitor {
    id.and_then(|id| {
        desktop_displays()
            .into_iter()
            .find(|display| id.eq_ignore_ascii_case(&display.id))
    })
    .and_then(|display| monitor_at(display.origin))
    .unwrap_or(primary)
}

/// Active Windows display targets and their persistent monitor device paths.
#[must_use]
pub fn desktop_displays() -> Vec<DesktopDisplay> {
    // The topology can change between sizing and filling the buffers.
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0, 0);
        let flags = QDC_ONLY_ACTIVE_PATHS;
        // SAFETY: Counts are writable and both arrays have the reported capacity.
        let (paths, modes) = unsafe {
            if GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) != ERROR_SUCCESS
            {
                break;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let status = QueryDisplayConfig(
                flags,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            );
            if status == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            if status != ERROR_SUCCESS {
                break;
            }
            paths.truncate(path_count as usize);
            modes.truncate(mode_count as usize);
            (paths, modes)
        };
        return paths
            .iter()
            .enumerate()
            .filter_map(|(index, path)| describe_display(path, &modes, index))
            .collect();
    }
    Vec::new()
}

fn describe_display(
    path: &DISPLAYCONFIG_PATH_INFO,
    modes: &[DISPLAYCONFIG_MODE_INFO],
    index: usize,
) -> Option<DesktopDisplay> {
    if !path.targetInfo.targetAvailable.as_bool() {
        return None;
    }
    // SAFETY: Without QDC_VIRTUAL_MODE_AWARE, modeInfoIdx is the active member.
    let mode = modes.get(unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize)?;
    if mode.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
        return None;
    }
    // SAFETY: This mode is tagged as a source mode.
    let position = unsafe { mode.Anonymous.sourceMode.position };
    let origin = POINT {
        x: position.x,
        y: position.y,
    };
    let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
            adapterId: path.targetInfo.adapterId,
            id: path.targetInfo.id,
        },
        ..Default::default()
    };
    // SAFETY: The header is the first field of the sized output packet.
    if unsafe { DisplayConfigGetDeviceInfo(&raw mut target.header) } != 0 {
        return None;
    }
    let id = wide_string(&target.monitorDevicePath);
    if id.is_empty() {
        return None;
    }
    let name = wide_string(&target.monitorFriendlyDeviceName);
    let name = if name.is_empty() {
        format!("#{}", index + 1)
    } else {
        name
    };
    Some(DesktopDisplay { id, name, origin })
}

fn wide_string(buffer: &[u16]) -> String {
    String::from_utf16_lossy(buffer.split(|&unit| unit == 0).next().unwrap_or_default())
}
