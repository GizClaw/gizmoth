use crate::credentials::{self, Device};
use btleplug::{
    api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType},
    platform::{Adapter, Manager, Peripheral},
};
use serde::Serialize;
use std::time::Duration;
use tokio::{
    sync::Mutex,
    time::{sleep, timeout},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const SERVICE: Uuid = Uuid::from_u128(0x14edd4b091a64b0d8c2fb12be4f83b36);
const INFO: Uuid = Uuid::from_u128(0x14edd4b191a64b0d8c2fb12be4f83b36);
const REQUEST: Uuid = Uuid::from_u128(0x14edd4b291a64b0d8c2fb12be4f83b36);
const CREDENTIAL: Uuid = Uuid::from_u128(0x14edd4b391a64b0d8c2fb12be4f83b36);
const PROTOCOL: &str = "设备绑定协议不匹配，请更新设备固件。";

#[derive(Default)]
pub struct BleState(pub Mutex<Option<Adapter>>);
#[derive(Serialize)]
pub struct Nearby {
    pub id: String,
    pub name: String,
}
#[derive(Debug)]
struct Info {
    flags: u8,
    revision: u64,
    length: usize,
    limit: usize,
}

fn decode_info(bytes: &[u8]) -> Result<Info, String> {
    if bytes.len() != 14 || bytes[0] != 1 {
        return Err(PROTOCOL.into());
    }
    let info = Info {
        flags: bytes[1],
        revision: u64::from_le_bytes(bytes[2..10].try_into().unwrap()),
        length: u16::from_le_bytes(bytes[10..12].try_into().unwrap()) as usize,
        limit: u16::from_le_bytes(bytes[12..14].try_into().unwrap()) as usize,
    };
    if ![0, 1, 2, 4, 8, 16].contains(&info.flags)
        || info.length > 1024
        || (info.flags == 1) != (info.length > 0)
        || !(1..=231).contains(&info.limit)
    {
        return Err(PROTOCOL.into());
    }
    match info.flags {
        4 => Err("设备绑定名额已用完".into()),
        8 => Err("设备已关闭绑定，请重新打开绑定窗口。".into()),
        16 => Err("设备生成凭据失败，请在设备上重试。".into()),
        _ => Ok(info),
    }
}

pub async fn scan(state: &BleState) -> Result<Vec<Nearby>, String> {
    let mut guard = state.0.try_lock().map_err(|_| "蓝牙任务正在进行")?;
    if guard.is_none() {
        let manager = Manager::new()
            .await
            .map_err(|_| "蓝牙不可用，请检查系统蓝牙权限。")?;
        *guard = manager
            .adapters()
            .await
            .map_err(|_| "无法读取蓝牙适配器")?
            .into_iter()
            .next();
    }
    let adapter = guard.as_ref().ok_or("未找到蓝牙适配器，请打开系统蓝牙。")?;
    adapter
        .start_scan(ScanFilter {
            services: vec![SERVICE],
        })
        .await
        .map_err(|_| "扫描失败，请检查系统蓝牙权限。")?;
    sleep(Duration::from_secs(5)).await;
    adapter.stop_scan().await.map_err(|_| "无法结束蓝牙扫描")?;
    let mut devices = vec![];
    for peripheral in adapter
        .peripherals()
        .await
        .map_err(|_| "无法读取蓝牙设备")?
    {
        if let Some(properties) = peripheral
            .properties()
            .await
            .map_err(|_| "无法读取蓝牙广播")?
        {
            if properties.services.contains(&SERVICE) {
                devices.push(Nearby {
                    id: peripheral.id().to_string(),
                    name: properties.local_name.unwrap_or_else(|| "GizOS 设备".into()),
                });
            }
        }
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}

async fn read(peripheral: &Peripheral) -> Result<credentials::Credential, String> {
    peripheral
        .connect()
        .await
        .map_err(|_| "无法连接设备，请靠近设备后重试。")?;
    peripheral
        .discover_services()
        .await
        .map_err(|_| "无法读取设备服务")?;
    let chars = peripheral.characteristics();
    let find = |uuid| {
        chars
            .iter()
            .find(|c| c.uuid == uuid && c.service_uuid == SERVICE)
            .ok_or(PROTOCOL.to_owned())
    };
    let info_char = find(INFO)?;
    let request = find(REQUEST)?;
    let credential = find(CREDENTIAL)?;
    loop {
        let info = decode_info(
            &peripheral
                .read(info_char)
                .await
                .map_err(|_| "无法读取绑定状态")?,
        )?;
        if info.flags != 1 {
            sleep(Duration::from_millis(250)).await;
            continue;
        }
        let mut buffer = Zeroizing::new(Vec::with_capacity(info.length));
        let mut restart = false;
        while buffer.len() < info.length {
            let current = decode_info(
                &peripheral
                    .read(info_char)
                    .await
                    .map_err(|_| "无法读取绑定状态")?,
            )?;
            if current.flags != 1
                || current.revision != info.revision
                || current.length != info.length
            {
                restart = true;
                break;
            }
            let mut packet = vec![1];
            packet.extend_from_slice(&info.revision.to_le_bytes());
            packet.extend_from_slice(&(buffer.len() as u16).to_le_bytes());
            packet.extend_from_slice(&(current.limit as u16).to_le_bytes());
            if peripheral
                .write(request, &packet, WriteType::WithResponse)
                .await
                .is_err()
            {
                let updated = decode_info(
                    &peripheral
                        .read(info_char)
                        .await
                        .map_err(|_| "无法读取绑定状态")?,
                )?;
                if updated.flags != 1 || updated.revision != info.revision {
                    restart = true;
                    break;
                }
                return Err("无法请求设备凭据".into());
            }
            let chunk = Zeroizing::new(
                peripheral
                    .read(credential)
                    .await
                    .map_err(|_| "无法读取设备凭据")?,
            );
            if chunk.len() < 13 || chunk.len() > 13 + current.limit || chunk[0] != 1 {
                return Err(PROTOCOL.into());
            }
            if u64::from_le_bytes(chunk[1..9].try_into().unwrap()) != info.revision {
                restart = true;
                break;
            }
            let offset = u16::from_le_bytes(chunk[9..11].try_into().unwrap()) as usize;
            let total = u16::from_le_bytes(chunk[11..13].try_into().unwrap()) as usize;
            if offset != buffer.len()
                || total != info.length
                || chunk.len() == 13
                || offset + chunk.len() - 13 > total
            {
                return Err(PROTOCOL.into());
            }
            buffer.extend_from_slice(&chunk[13..]);
        }
        if restart {
            sleep(Duration::from_millis(250)).await;
            continue;
        }
        return credentials::parse(std::str::from_utf8(&buffer).map_err(|_| PROTOCOL)?);
    }
}

pub async fn bind(app: &tauri::AppHandle, state: &BleState, id: &str) -> Result<Device, String> {
    let guard = state.0.try_lock().map_err(|_| "蓝牙任务正在进行")?;
    let adapter = guard.as_ref().ok_or("请先扫描设备")?;
    let peripheral = adapter
        .peripherals()
        .await
        .map_err(|_| "无法读取设备")?
        .into_iter()
        .find(|p| p.id().to_string() == id)
        .ok_or("设备已离开，请重新扫描。")?;
    let result = timeout(Duration::from_secs(30), read(&peripheral))
        .await
        .map_err(|_| "绑定超时，请确认设备已打开绑定窗口。".to_owned())
        .and_then(|r| r);
    let _ = timeout(Duration::from_secs(3), peripheral.disconnect()).await;
    credentials::import(app, result?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_binding_info_before_allocating() {
        assert!(decode_info(&[1; 13]).is_err());
        let mut bytes = [0; 14];
        bytes[0] = 1;
        bytes[1] = 1;
        bytes[10] = 20;
        bytes[12] = 231;
        let info = decode_info(&bytes).unwrap();
        assert_eq!(info.length, 20);
        bytes[1] = 3;
        assert!(decode_info(&bytes).is_err());
        bytes[1] = 1;
        bytes[12] = 232;
        assert!(decode_info(&bytes).is_err());
        bytes[12] = 231;
        bytes[11] = 5;
        assert!(decode_info(&bytes).is_err());
    }
}
