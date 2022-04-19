use std::cell::Ref;
use std::cell::RefCell;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::rc::Rc;

use crate::Cell;
use crate::Hive;
use crate::subkeys_list::*;
use crate::Offset;
use crate::vk::KeyValueList;
use crate::vk::KeyValue;
use binread::BinResult;
use binread::FilePtr32;
use binread::ReadOptions;
use binread::derive_binread;
use binread::{BinReaderExt};
use bitflags::bitflags;
use chrono::DateTime;
use chrono::Utc;
use crate::util::{parse_string, parse_timestamp};

#[allow(dead_code)]
#[derive_binread]
#[br(magic = b"nk")]
pub struct KeyNode {
    #[br(parse_with=parse_node_flags)]
    flags: KeyNodeFlags,
    
    #[br(parse_with=parse_timestamp)]
    timestamp: DateTime<Utc>,
    access_bits: u32,
    parent: u32,
    subkey_count: u32,

    #[br(temp)]
    volatile_subkey_count: u32,
    subkeys_list_offset: Offset,

    #[br(temp)]
    volatile_subkeys_list_offset: Offset,

    #[br(temp)]
    key_values_count: u32,

    #[br(   if(key_values_count > 0),
            deref_now,
            restore_position,
            args(key_values_count as usize))]
    key_values_list: Option<FilePtr32<Cell<KeyValueList, (usize,)>>>,

    #[br(temp)]
    key_values_list_offset: u32,

    #[br(temp)]
    key_security_offset: Offset,
    
    #[br(temp)]
    class_name_offset: Offset,

    #[br(temp)]
    max_subkey_name: u32,

    #[br(temp)]
    max_subkey_class_name: u32,

    #[br(temp)]
    max_value_name: u32,

    #[br(temp)]
    max_value_data: u32,

    #[br(temp)]
    work_var: u32,

    #[br(temp)]
    key_name_length: u16,

    #[br(temp)]
    class_name_length: u16,

    #[br(   parse_with=parse_string,
            count=key_name_length,
            args(flags.contains(KeyNodeFlags::KEY_COMP_NAME)))]
    key_name_string: String,

    #[br(   if(key_values_count > 0 && key_values_list_offset != u32::MAX),
            parse_with=read_values,
            args(key_values_list.as_ref(), ))]
    values: Vec<KeyValue>,

    #[br(default)]
    subkeys: Rc<RefCell<Vec<Rc<RefCell<Self>>>>>
}

fn parse_node_flags<R: Read + Seek>(reader: &mut R, _ro: &ReadOptions, _: ())
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

impl KeyNode
{
    /// Returns the name of this Key Node.
    pub fn name(&self) -> &str {
        &self.key_name_string
    }

    pub fn timestamp(&self) -> &DateTime<Utc> {
        &self.timestamp
    }

    pub fn subkey_count(&self) -> u32 {
        self.subkey_count
    }

    pub fn subkeys<B>(&self, hive: &mut Hive<B>) -> BinResult<Ref<Vec<Rc<RefCell<Self>>>>> where B: BinReaderExt {
        if self.subkeys.borrow().is_empty() && self.subkey_count() > 0 {
            let sk = self.read_subkeys(hive)?;
            *self.subkeys.borrow_mut() = sk;
        }
        Ok(self.subkeys.borrow())
    }

    fn read_subkeys<B>(&self, hive: &mut Hive<B>) -> BinResult<Vec<Rc<RefCell<Self>>>> where B: BinReaderExt {
        let offset = self.subkeys_list_offset;

        if offset.0 == u32::MAX{
            return Ok(Vec::new());
        }

        let subkeys_list: SubKeysList = hive.read_structure(offset)?;

        log::debug!("SubKeyList is of type '{}'", match subkeys_list {
            SubKeysList::IndexLeaf { items: _, ..} => "IndexLeaf",
            SubKeysList::FastLeaf { items: _, ..} => "FastLeaf",
            SubKeysList::HashLeaf { items: _, ..} => "HashLeaf",
            SubKeysList::IndexRoot { items: _, ..} => "IndexRoot",
        });

        log::trace!("{:?}", subkeys_list);

        if subkeys_list.is_index_root() {
            log::debug!("reading indirect subkey lists");
            let subkeys: BinResult<Vec<_>>= subkeys_list.into_offsets().map(|o| {
                let subsubkeys_list: SubKeysList = hive.read_structure(o)?;
                assert!(!subsubkeys_list.is_index_root());

                let subkeys: BinResult<Vec<_>> = subsubkeys_list.into_offsets().map(|o2| {
                    let nk: KeyNode = hive.read_structure(o2)?;
                    Ok(Rc::new(RefCell::new(nk)))
                }).collect();
                subkeys
            }).collect();

            match subkeys {
                Err(why) => Err(why),
                Ok(sk) => Ok(sk.into_iter().flatten().collect())
            }
        } else {
            log::debug!("reading single subkey list");
            let subkeys: BinResult<Vec<_>> = subkeys_list.into_offsets().map(|offset| {
                let nk: KeyNode = hive.read_structure(offset)?;
                Ok(Rc::new(RefCell::new(nk)))
            }).collect();
            subkeys
        }
    }
    

    fn subpath_parts<B>(&self, mut path_parts: Vec<&str>, hive: &mut Hive<B>) -> BinResult<Option<Rc<RefCell<Self>>>> where B: BinReaderExt {
        eprintln!("subpath_parts({:?}): BEGIN", path_parts);
        if let Some(first) = path_parts.pop() {
            if let Some(top) = self.subkey(first, hive)? {
                return if path_parts.is_empty() {
                    Ok(Some(top))
                } else {
                    top.borrow().subpath_parts(path_parts, hive)
                };
            }
        }
        Ok(None)
    }

    pub fn subkey<B>(&self, name: &str, hive: &mut Hive<B>) -> BinResult<Option<Rc<RefCell<Self>>>> where B: BinReaderExt {
        let subkey = self.subkeys(hive)?
            .iter()
            .find(|s|s.borrow().name() == name)
            .map(|kn| Rc::clone(kn));
        Ok(subkey)
    }


    pub fn values(&self) -> &Vec<KeyValue> {
        &self.values
    }
}

pub trait SubPath<T> {
    fn subpath<B>(&self, path: T, hive: &mut Hive<B>) -> BinResult<Option<Rc<RefCell<Self>>>> where B: BinReaderExt;
}

impl SubPath<&str> for KeyNode {
    fn subpath<B>(&self, path: &str, hive: &mut Hive<B>) -> BinResult<Option<Rc<RefCell<Self>>>> where B: BinReaderExt {
        let path_parts: Vec<_> = path.split('\\').rev().collect();
        self.subpath_parts(path_parts, hive)
    }
}

impl SubPath<&String> for KeyNode {
    fn subpath<B>(&self, path: &String, hive: &mut Hive<B>) -> BinResult<Option<Rc<RefCell<Self>>>> where B: BinReaderExt {
        let path_parts: Vec<_> = path.split('\\').rev().collect();
        self.subpath_parts(path_parts, hive)
    }
}

impl SubPath<&Vec<&str>> for KeyNode {
    fn subpath<B>(&self, path: &Vec<&str>, hive: &mut Hive<B>) -> BinResult<Option<Rc<RefCell<Self>>>> where B: BinReaderExt {
        let path_parts: Vec<_> = path.iter().rev().map(|s| *s).collect();
        self.subpath_parts(path_parts, hive)
    }
}

impl SubPath<&Vec<String>> for KeyNode {
    fn subpath<B>(&self, path: &Vec<String>, hive: &mut Hive<B>) -> BinResult<Option<Rc<RefCell<Self>>>> where B: BinReaderExt {
        let path_parts: Vec<_> = path.iter().rev().map(|s| &s[..]).collect();
        self.subpath_parts(path_parts, hive)
    }
}


fn read_values<R: Read + Seek>(
    reader: &mut R,
    _ro: &ReadOptions,
    args: (Option<&FilePtr32<Cell<KeyValueList, (usize,)>>>, ),
) -> BinResult<Vec<KeyValue>> {
    Ok(match args.0 {
        None => Vec::new(),
        Some(key_values_list) => match &key_values_list.value {
            None => Vec::new(),
            Some(kv_list_cell) => {
                let kv_list: &KeyValueList = kv_list_cell.data();
                let mut result = Vec::with_capacity(kv_list.key_value_offsets.len() as usize);
                for offset in kv_list.key_value_offsets.iter() {
                    reader.seek(SeekFrom::Start(offset.0.into()))?;
                    let vk: Cell<KeyValue, ()> = reader.read_le().unwrap();
                    result.push(vk.into());
                }
                result
            }
        }
    })
}

impl From<Cell<KeyNode, ()>> for KeyNode {
    fn from(cell: Cell<KeyNode, ()>) -> Self {
        cell.into_data()
    }
}

#[cfg(test)]
mod tests {
    use crate::*;
    use std::io;

    #[test]
    fn enum_subkeys() {
        let testhive = crate::helpers::tests::testhive_vec();
        let mut hive = Hive::new(io::Cursor::new(testhive)).unwrap();
        assert!(hive.enum_subkeys(|hive, k: &KeyNode| {
            assert_eq!(k.name(), "ROOT");

            for sk in k.subkeys(hive).unwrap().iter() {
                println!("{}", sk.borrow().name());
            }

            Ok(())
        }).is_ok());
    }
}

