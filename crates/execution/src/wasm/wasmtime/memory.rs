//! Checked guest-memory codecs.

use super::store::WasmtimeStoreState;
use std::ops::Range;
use wasmtime::{Caller, Extern, Memory};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemoryError {
    MissingMemory,
    AddressOverflow,
    OutOfBounds,
    InvalidUtf8,
}

impl std::fmt::Display for GuestMemoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MissingMemory => "guest module does not export linear memory as `memory`",
            Self::AddressOverflow => "guest memory range overflows the host address space",
            Self::OutOfBounds => "guest memory range is out of bounds",
            Self::InvalidUtf8 => "guest string is not valid UTF-8",
        })
    }
}

impl std::error::Error for GuestMemoryError {}

pub fn exported_memory(
    caller: &mut Caller<'_, WasmtimeStoreState>,
) -> Result<Memory, GuestMemoryError> {
    match caller.get_export("memory") {
        Some(Extern::Memory(memory)) => Ok(memory),
        _ => Err(GuestMemoryError::MissingMemory),
    }
}

pub fn validate_range(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    length: usize,
) -> Result<(Memory, Range<usize>), GuestMemoryError> {
    let memory = exported_memory(caller)?;
    let start = usize::try_from(pointer).map_err(|_| GuestMemoryError::AddressOverflow)?;
    let end = start
        .checked_add(length)
        .ok_or(GuestMemoryError::AddressOverflow)?;
    if end > memory.data_size(&mut *caller) {
        return Err(GuestMemoryError::OutOfBounds);
    }
    Ok((memory, start..end))
}

pub fn read_bytes(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    length: usize,
) -> Result<Vec<u8>, GuestMemoryError> {
    let (memory, range) = validate_range(caller, pointer, length)?;
    Ok(memory.data(&mut *caller)[range].to_vec())
}

pub fn read_string(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    length: usize,
) -> Result<String, GuestMemoryError> {
    String::from_utf8(read_bytes(caller, pointer, length)?)
        .map_err(|_| GuestMemoryError::InvalidUtf8)
}

pub fn write_bytes(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    bytes: &[u8],
) -> Result<(), GuestMemoryError> {
    let (memory, range) = validate_range(caller, pointer, bytes.len())?;
    memory.data_mut(&mut *caller)[range].copy_from_slice(bytes);
    Ok(())
}

pub fn write_u32(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    value: u32,
) -> Result<(), GuestMemoryError> {
    write_bytes(caller, pointer, &value.to_le_bytes())
}

pub fn write_u64(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
    value: u64,
) -> Result<(), GuestMemoryError> {
    write_bytes(caller, pointer, &value.to_le_bytes())
}

pub fn read_u32(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
) -> Result<u32, GuestMemoryError> {
    let bytes = read_bytes(caller, pointer, 4)?;
    Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
}

pub fn read_u64(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointer: u32,
) -> Result<u64, GuestMemoryError> {
    let bytes = read_bytes(caller, pointer, 8)?;
    Ok(u64::from_le_bytes(bytes.try_into().expect("eight bytes")))
}

pub fn validate_string_table_outputs(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointers: u32,
    buffer: u32,
    strings: &[Vec<u8>],
) -> Result<(), GuestMemoryError> {
    let pointer_bytes = strings
        .len()
        .checked_mul(4)
        .ok_or(GuestMemoryError::AddressOverflow)?;
    let buffer_bytes = strings.iter().try_fold(0usize, |total, value| {
        total
            .checked_add(value.len())
            .ok_or(GuestMemoryError::AddressOverflow)
    })?;
    validate_range(caller, pointers, pointer_bytes)?;
    validate_range(caller, buffer, buffer_bytes)?;
    Ok(())
}

pub fn write_string_table(
    caller: &mut Caller<'_, WasmtimeStoreState>,
    pointers: u32,
    buffer: u32,
    strings: &[Vec<u8>],
) -> Result<(), GuestMemoryError> {
    validate_string_table_outputs(caller, pointers, buffer, strings)?;
    let mut offset = 0usize;
    for (index, value) in strings.iter().enumerate() {
        let value_pointer = usize::try_from(buffer)
            .ok()
            .and_then(|base| base.checked_add(offset))
            .and_then(|pointer| u32::try_from(pointer).ok())
            .ok_or(GuestMemoryError::AddressOverflow)?;
        let pointer_slot = usize::try_from(pointers)
            .ok()
            .and_then(|base| base.checked_add(index.saturating_mul(4)))
            .and_then(|pointer| u32::try_from(pointer).ok())
            .ok_or(GuestMemoryError::AddressOverflow)?;
        write_u32(caller, pointer_slot, value_pointer)?;
        write_bytes(caller, value_pointer, value)?;
        offset = offset
            .checked_add(value.len())
            .ok_or(GuestMemoryError::AddressOverflow)?;
    }
    Ok(())
}
