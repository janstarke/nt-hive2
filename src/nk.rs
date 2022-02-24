use std::borrow::Cow;
use std::io::Read;
use std::io::Seek;
use std::ops::Deref;

use crate::Hive;
use crate::NtHiveError;
use crate::Result;
use binread::BinResult;
use binread::ReadOptions;
use binread::{BinRead, BinReaderExt};
use bitflags::bitflags;
use encoding_rs::{ISO_8859_15, UTF_16LE};

#[allow(dead_code)]
#[derive(BinRead)]
#[br(magic = b"nk")]
pub(crate) struct KeyNodeHeader {
    #[br(parse_with=parse_node_flags)]
    flags: KeyNodeFlags,
    timestamp: u64,
    spare: u32,
    parent: u32,
    subkey_count: u32,
    volatile_subkey_count: u32,
    subkeys_list_offset: u32,
    volatile_subkeys_list_offset: u32,
    key_values_count: u32,
    key_values_list_offset: u32,
    key_security_offset: u32,
    class_name_offset: u32,
    max_subkey_name: u32,
    max_subkey_class_name: u32,
    max_value_name: u32,
    max_value_data: u32,
    work_var: u32,
    key_name_length: u16,
    class_name_length: u16,

    #[br(count=key_name_length)]
    key_name_string: Vec<u8>,
}

fn parse_node_flags<R: Read + Seek>(reader: &mut R, ro: &ReadOptions, _: ())
-> BinResult<KeyNodeFlags>
{
    let raw_value: u16 = reader.read_le()?;
    Ok(KeyNodeFlags::from_bits(raw_value).unwrap())
}

bitflags! {
    struct KeyNodeFlags: u16 {
        /// This is a volatile key (not stored on disk).
        const KEY_IS_VOLATILE = 0x0001;
        /// This is the mount point of another hive (not stored on disk).
        const KEY_HIVE_EXIT = 0x0002;
        /// This is the root key.
        const KEY_HIVE_ENTRY = 0x0004;
        /// This key cannot be deleted.
        const KEY_NO_DELETE = 0x0008;
        /// This key is a symbolic link.
        const KEY_SYM_LINK = 0x0010;
        /// The key name is in (extended) ASCII instead of UTF-16LE.
        const KEY_COMP_NAME = 0x0020;
        /// This key is a predefined handle.
        const KEY_PREDEF_HANDLE = 0x0040;
        /// This key was virtualized at least once.
        const KEY_VIRT_MIRRORED = 0x0080;
        /// This is a virtual key.
        const KEY_VIRT_TARGET = 0x0100;
        /// This key is part of a virtual store path.
        const KEY_VIRTUAL_STORE = 0x0200;
    }
}


pub struct KeyNode<H, B>
where
    H: Deref<Target = Hive<B>>,
    B: BinReaderExt,
{
    header: KeyNodeHeader,
    hive: H,
}

impl<H, B> KeyNode<H, B>
where
    H: Deref<Target = Hive<B>>,
    B: BinReaderExt,
{
    pub fn from_cell_offset(hive: H, offset: u32) -> Result<Self> {
        hive.seek_to_cell_offset(offset)?;
        let header: KeyNodeHeader = hive.data.borrow_mut().read_le().unwrap();
        Ok(Self { header, hive })
    }

    /// Returns the name of this Key Node.
    pub fn name(&self) -> Result<Cow<str>> {
        let (cow, _, had_errors) = 
        if self.header.flags.contains(KeyNodeFlags::KEY_COMP_NAME) {
            ISO_8859_15.decode(&self.header.key_name_string[..])
        } else {
            UTF_16LE.decode(&self.header.key_name_string[..])
        };

        if had_errors {
            Err(NtHiveError::StringEncodingError)
        } else {
            Ok(cow)
        }
    }
}