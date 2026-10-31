// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Dump ELF headers of executable and object files.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::process::ExitCode;

const USAGE: &str = "Usage: elfdump [-ahlS] file...";
const ELF_MAGIC: &[u8] = b"\x7fELF";
const IDENT_SIZE: usize = 16;
const ELFCLASS32: u8 = 1;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ELFDATA2MSB: u8 = 2;
const EV_CURRENT: u8 = 1;
const HEADER_SIZE_32: u16 = 52;
const HEADER_SIZE_64: u16 = 64;
const PROGRAM_HEADER_SIZE_32: u16 = 32;
const PROGRAM_HEADER_SIZE_64: u16 = 56;
const SECTION_HEADER_SIZE_32: u16 = 40;
const SECTION_HEADER_SIZE_64: u16 = 64;
const MAX_PROGRAM_HEADERS: usize = 128;
const MAX_SECTION_HEADERS: usize = 1024;
const MAX_STRING_TABLE_SIZE: usize = 1024 * 1024;
const SHN_XINDEX: u16 = 0xffff;
const SHN_UNDEF: u16 = 0;

const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;
const EI_VERSION: usize = 6;
const EI_OSABI: usize = 7;
const EI_ABIVERSION: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Options {
    header: bool,
    program_headers: bool,
    section_headers: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { header: true, program_headers: true, section_headers: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ident {
    class64: bool,
    little_endian: bool,
    os_abi: u8,
    abi_version: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileHeader {
    file_type: u16,
    machine: u16,
    version: u32,
    entry: u64,
    program_header_offset: u64,
    section_header_offset: u64,
    flags: u32,
    header_size: u16,
    program_header_entry_size: u16,
    program_header_count: u16,
    section_header_entry_size: u16,
    section_header_count: u16,
    section_name_string_index: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProgramHeader {
    segment_type: u32,
    flags: u32,
    offset: u64,
    virtual_address: u64,
    physical_address: u64,
    file_size: u64,
    memory_size: u64,
    align: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SectionHeader {
    name: String,
    section_type: u32,
    flags: u64,
    address: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
    address_align: u64,
    entry_size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ElfFile {
    ident_bytes: [u8; IDENT_SIZE],
    ident: Ident,
    header: FileHeader,
    program_headers: Vec<ProgramHeader>,
    section_headers: Vec<SectionHeader>,
}

struct ByteReader<'a> {
    data: &'a [u8],
    little_endian: bool,
}

impl<'a> ByteReader<'a> {
    fn new(data: &'a [u8], little_endian: bool) -> Self {
        Self { data, little_endian }
    }

    fn read_bytes(&self, offset: usize, length: usize) -> Result<&'a [u8], String> {
        let end = offset.checked_add(length).ok_or_else(|| "truncated ELF data".to_string())?;
        self.data.get(offset..end).ok_or_else(|| "truncated ELF data".to_string())
    }

    fn read_u16(&self, offset: usize) -> Result<u16, String> {
        let bytes = self.read_bytes(offset, 2)?;
        let value = [bytes[0], bytes[1]];
        Ok(if self.little_endian { u16::from_le_bytes(value) } else { u16::from_be_bytes(value) })
    }

    fn read_u32(&self, offset: usize) -> Result<u32, String> {
        let bytes = self.read_bytes(offset, 4)?;
        let value = [bytes[0], bytes[1], bytes[2], bytes[3]];
        Ok(if self.little_endian { u32::from_le_bytes(value) } else { u32::from_be_bytes(value) })
    }

    fn read_u64(&self, offset: usize) -> Result<u64, String> {
        let bytes = self.read_bytes(offset, 8)?;
        let mut value = [0u8; 8];
        value.copy_from_slice(bytes);
        Ok(if self.little_endian { u64::from_le_bytes(value) } else { u64::from_be_bytes(value) })
    }

    fn read_addr(&self, offset: usize, class64: bool) -> Result<u64, String> {
        if class64 { self.read_u64(offset) } else { Ok(u64::from(self.read_u32(offset)?)) }
    }
}

fn parse_ident(bytes: &[u8; IDENT_SIZE]) -> Result<Ident, String> {
    if bytes[..4] != *ELF_MAGIC {
        return Err("not an ELF file".to_string());
    }
    let class64 = match bytes[EI_CLASS] {
        ELFCLASS32 => false,
        ELFCLASS64 => true,
        class => return Err(format!("unsupported ELF class {class}")),
    };
    let little_endian = match bytes[EI_DATA] {
        ELFDATA2LSB => true,
        ELFDATA2MSB => false,
        encoding => return Err(format!("unsupported ELF encoding {encoding}")),
    };
    if bytes[EI_VERSION] != EV_CURRENT {
        return Err(format!("unsupported ELF version {}", bytes[EI_VERSION]));
    }
    Ok(Ident { class64, little_endian, os_abi: bytes[EI_OSABI], abi_version: bytes[EI_ABIVERSION] })
}

fn parse_file_header(ident: Ident, data: &[u8]) -> Result<FileHeader, String> {
    let reader = ByteReader::new(data, ident.little_endian);
    let expected_header_size = if ident.class64 { HEADER_SIZE_64 } else { HEADER_SIZE_32 };
    if data.len() < expected_header_size as usize {
        return Err("truncated ELF header".to_string());
    }

    let (entry, program_header_offset, section_header_offset, flags_offset) = if ident.class64 {
        (reader.read_u64(24)?, reader.read_u64(32)?, reader.read_u64(40)?, 48)
    } else {
        (
            reader.read_addr(24, false)?,
            reader.read_addr(28, false)?,
            reader.read_addr(32, false)?,
            36,
        )
    };

    Ok(FileHeader {
        file_type: reader.read_u16(16)?,
        machine: reader.read_u16(18)?,
        version: reader.read_u32(20)?,
        entry,
        program_header_offset,
        section_header_offset,
        flags: reader.read_u32(flags_offset)?,
        header_size: reader.read_u16(flags_offset + 4)?,
        program_header_entry_size: reader.read_u16(flags_offset + 6)?,
        program_header_count: reader.read_u16(flags_offset + 8)?,
        section_header_entry_size: reader.read_u16(flags_offset + 10)?,
        section_header_count: reader.read_u16(flags_offset + 12)?,
        section_name_string_index: reader.read_u16(flags_offset + 14)?,
    })
}

fn parse_program_header(ident: Ident, data: &[u8]) -> Result<ProgramHeader, String> {
    let reader = ByteReader::new(data, ident.little_endian);
    if ident.class64 {
        Ok(ProgramHeader {
            segment_type: reader.read_u32(0)?,
            flags: reader.read_u32(4)?,
            offset: reader.read_u64(8)?,
            virtual_address: reader.read_u64(16)?,
            physical_address: reader.read_u64(24)?,
            file_size: reader.read_u64(32)?,
            memory_size: reader.read_u64(40)?,
            align: reader.read_u64(48)?,
        })
    } else {
        Ok(ProgramHeader {
            segment_type: reader.read_u32(0)?,
            offset: reader.read_addr(4, false)?,
            virtual_address: reader.read_addr(8, false)?,
            physical_address: reader.read_addr(12, false)?,
            file_size: reader.read_addr(16, false)?,
            memory_size: reader.read_addr(20, false)?,
            flags: reader.read_u32(24)?,
            align: reader.read_addr(28, false)?,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct RawSectionHeader {
    name_offset: u32,
    section_type: u32,
    flags: u64,
    address: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
    address_align: u64,
    entry_size: u64,
}

fn parse_section_header_fields(ident: Ident, data: &[u8]) -> Result<RawSectionHeader, String> {
    let reader = ByteReader::new(data, ident.little_endian);
    if ident.class64 {
        Ok(RawSectionHeader {
            name_offset: reader.read_u32(0)?,
            section_type: reader.read_u32(4)?,
            flags: reader.read_u64(8)?,
            address: reader.read_u64(16)?,
            offset: reader.read_u64(24)?,
            size: reader.read_u64(32)?,
            link: reader.read_u32(40)?,
            info: reader.read_u32(44)?,
            address_align: reader.read_u64(48)?,
            entry_size: reader.read_u64(56)?,
        })
    } else {
        Ok(RawSectionHeader {
            name_offset: reader.read_u32(0)?,
            section_type: reader.read_u32(4)?,
            flags: u64::from(reader.read_u32(8)?),
            address: reader.read_addr(12, false)?,
            offset: reader.read_addr(16, false)?,
            size: reader.read_addr(20, false)?,
            link: reader.read_u32(24)?,
            info: reader.read_u32(28)?,
            address_align: reader.read_addr(32, false)?,
            entry_size: reader.read_addr(36, false)?,
        })
    }
}

fn expected_program_header_size(class64: bool) -> u16 {
    if class64 { PROGRAM_HEADER_SIZE_64 } else { PROGRAM_HEADER_SIZE_32 }
}

fn expected_section_header_size(class64: bool) -> u16 {
    if class64 { SECTION_HEADER_SIZE_64 } else { SECTION_HEADER_SIZE_32 }
}

fn read_exact_at(file: &mut File, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buffer)
}

fn string_from_table(table: &[u8], offset: u32) -> String {
    let start = offset as usize;
    if start >= table.len() {
        return String::new();
    }
    let end = table[start..]
        .iter()
        .position(|&byte| byte == 0)
        .map(|index| start + index)
        .unwrap_or(table.len());
    String::from_utf8_lossy(&table[start..end]).into_owned()
}

fn load_elf(path: &str) -> Result<ElfFile, String> {
    let metadata = std::fs::metadata(path).map_err(|err| err.to_string())?;
    if metadata.is_dir() {
        return Err("Is a directory".to_string());
    }

    let mut file = File::open(path).map_err(|err| err.to_string())?;
    let mut ident_bytes = [0u8; IDENT_SIZE];
    file.read_exact(&mut ident_bytes).map_err(|_| "truncated ELF header".to_string())?;
    let ident = parse_ident(&ident_bytes)?;

    let header_size = if ident.class64 { HEADER_SIZE_64 } else { HEADER_SIZE_32 };
    let mut header_bytes = vec![0u8; header_size as usize];
    header_bytes[..IDENT_SIZE].copy_from_slice(&ident_bytes);
    read_exact_at(&mut file, IDENT_SIZE as u64, &mut header_bytes[IDENT_SIZE..])
        .map_err(|_| "truncated ELF header".to_string())?;
    let mut header = parse_file_header(ident, &header_bytes)?;
    if header.header_size != header_size {
        return Err(format!("unexpected ELF header size {}", header.header_size));
    }

    let mut program_headers = Vec::new();
    if header.program_header_count > 0 {
        let expected = expected_program_header_size(ident.class64);
        if header.program_header_entry_size != expected {
            return Err(format!(
                "unexpected program header size {}",
                header.program_header_entry_size
            ));
        }
        let count = header.program_header_count as usize;
        if count > MAX_PROGRAM_HEADERS {
            return Err(format!("too many program headers ({count})"));
        }
        let mut entry = vec![0u8; expected as usize];
        for index in 0..count {
            let offset = header
                .program_header_offset
                .checked_add((index as u64).saturating_mul(expected as u64))
                .ok_or_else(|| "invalid program header offset".to_string())?;
            read_exact_at(&mut file, offset, &mut entry)
                .map_err(|_| format!("truncated program header {index}"))?;
            program_headers.push(parse_program_header(ident, &entry)?);
        }
    }

    let mut raw_sections = Vec::new();
    if header.section_header_offset != 0 {
        let expected = expected_section_header_size(ident.class64);
        if header.section_header_entry_size != 0 && header.section_header_entry_size != expected {
            return Err(format!(
                "unexpected section header size {}",
                header.section_header_entry_size
            ));
        }
        let mut count = header.section_header_count as usize;
        if header.section_header_count == 0 {
            count = 1;
        }
        if count > MAX_SECTION_HEADERS {
            return Err(format!("too many section headers ({count})"));
        }
        let mut entry = vec![0u8; expected as usize];
        for index in 0..count {
            let offset = header
                .section_header_offset
                .checked_add((index as u64).saturating_mul(expected as u64))
                .ok_or_else(|| "invalid section header offset".to_string())?;
            read_exact_at(&mut file, offset, &mut entry)
                .map_err(|_| format!("truncated section header {index}"))?;
            raw_sections.push(parse_section_header_fields(ident, &entry)?);
        }
        if header.section_header_count == 0 {
            let actual = raw_sections[0].size as usize;
            if actual == 0 || actual > MAX_SECTION_HEADERS {
                return Err(format!("invalid extended section count {actual}"));
            }
            header.section_header_count = actual as u16;
            for index in 1..actual {
                let offset = header
                    .section_header_offset
                    .checked_add((index as u64).saturating_mul(expected as u64))
                    .ok_or_else(|| "invalid section header offset".to_string())?;
                read_exact_at(&mut file, offset, &mut entry)
                    .map_err(|_| format!("truncated section header {index}"))?;
                raw_sections.push(parse_section_header_fields(ident, &entry)?);
            }
        }
        if header.section_name_string_index == SHN_XINDEX {
            header.section_name_string_index = raw_sections[0].link as u16;
        }
    }

    let string_table = if header.section_name_string_index != SHN_UNDEF
        && (header.section_name_string_index as usize) < raw_sections.len()
    {
        let table = raw_sections[header.section_name_string_index as usize];
        let size = table.size as usize;
        if size > MAX_STRING_TABLE_SIZE {
            return Err("section name string table is too large".to_string());
        }
        let mut bytes = vec![0u8; size];
        if size > 0 {
            read_exact_at(&mut file, table.offset, &mut bytes)
                .map_err(|_| "truncated section name string table".to_string())?;
        }
        bytes
    } else {
        Vec::new()
    };

    let section_headers = raw_sections
        .into_iter()
        .map(|fields| SectionHeader {
            name: string_from_table(&string_table, fields.name_offset),
            section_type: fields.section_type,
            flags: fields.flags,
            address: fields.address,
            offset: fields.offset,
            size: fields.size,
            link: fields.link,
            info: fields.info,
            address_align: fields.address_align,
            entry_size: fields.entry_size,
        })
        .collect();

    Ok(ElfFile { ident_bytes, ident, header, program_headers, section_headers })
}

fn class_name(class64: bool) -> &'static str {
    if class64 { "ELF64" } else { "ELF32" }
}

fn encoding_name(little_endian: bool) -> &'static str {
    if little_endian { "2's complement, little endian" } else { "2's complement, big endian" }
}

fn os_abi_name(os_abi: u8) -> &'static str {
    match os_abi {
        0 => "UNIX - System V",
        3 => "UNIX - Linux",
        6 => "UNIX - Solaris",
        9 => "UNIX - FreeBSD",
        _ => "unknown",
    }
}

fn file_type_name(file_type: u16) -> &'static str {
    match file_type {
        0 => "NONE (None)",
        1 => "REL (Relocatable file)",
        2 => "EXEC (Executable file)",
        3 => "DYN (Shared object file)",
        4 => "CORE (Core file)",
        _ => "unknown",
    }
}

fn machine_name(machine: u16) -> &'static str {
    match machine {
        0 => "None",
        3 => "Intel 80386",
        40 => "ARM",
        62 => "Advanced Micro Devices X86-64",
        183 => "AArch64",
        243 => "RISC-V",
        _ => "unknown",
    }
}

fn program_header_type_name(segment_type: u32) -> String {
    match segment_type {
        0 => "NULL".to_string(),
        1 => "LOAD".to_string(),
        2 => "DYNAMIC".to_string(),
        3 => "INTERP".to_string(),
        4 => "NOTE".to_string(),
        5 => "SHLIB".to_string(),
        6 => "PHDR".to_string(),
        7 => "TLS".to_string(),
        0x6474e550 => "GNU_EH_FRAME".to_string(),
        0x6474e551 => "GNU_STACK".to_string(),
        0x6474e552 => "GNU_RELRO".to_string(),
        0x6474e553 => "GNU_PROPERTY".to_string(),
        other => format!("0x{other:x}"),
    }
}

fn program_header_flags(flags: u32) -> String {
    let read = if flags & 4 != 0 { 'r' } else { '-' };
    let write = if flags & 2 != 0 { 'w' } else { '-' };
    let execute = if flags & 1 != 0 { 'x' } else { '-' };
    format!("{read}{write}{execute}")
}

fn section_type_name(section_type: u32) -> String {
    match section_type {
        0 => "NULL".to_string(),
        1 => "PROGBITS".to_string(),
        2 => "SYMTAB".to_string(),
        3 => "STRTAB".to_string(),
        4 => "RELA".to_string(),
        5 => "HASH".to_string(),
        6 => "DYNAMIC".to_string(),
        7 => "NOTE".to_string(),
        8 => "NOBITS".to_string(),
        9 => "REL".to_string(),
        11 => "DYNSYM".to_string(),
        other => format!("0x{other:x}"),
    }
}

fn section_flag_text(flags: u64) -> String {
    let mut text = String::new();
    if flags & 0x1 != 0 {
        text.push('W');
    }
    if flags & 0x2 != 0 {
        text.push('A');
    }
    if flags & 0x4 != 0 {
        text.push('X');
    }
    if flags & 0x10 != 0 {
        text.push('M');
    }
    if flags & 0x20 != 0 {
        text.push('S');
    }
    if flags & 0x40 != 0 {
        text.push('I');
    }
    if flags & 0x400 != 0 {
        text.push('T');
    }
    text
}

fn write_ident_bytes<W: Write>(writer: &mut W, bytes: &[u8; IDENT_SIZE]) -> io::Result<()> {
    write!(writer, "  Magic:   ")?;
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            write!(writer, " ")?;
        }
        write!(writer, "{byte:02x}")?;
    }
    writeln!(writer)
}

fn write_header<W: Write>(writer: &mut W, elf: &ElfFile) -> io::Result<()> {
    writeln!(writer, "ELF Header:")?;
    write_ident_bytes(writer, &elf.ident_bytes)?;
    writeln!(writer, "  Class:                             {}", class_name(elf.ident.class64))?;
    writeln!(
        writer,
        "  Data:                              {}",
        encoding_name(elf.ident.little_endian)
    )?;
    writeln!(
        writer,
        "  Version:                           {} (current)",
        elf.ident_bytes[EI_VERSION]
    )?;
    let abi = os_abi_name(elf.ident.os_abi);
    if abi == "unknown" {
        writeln!(writer, "  OS/ABI:                            unknown ({})", elf.ident.os_abi)?;
    } else {
        writeln!(writer, "  OS/ABI:                            {abi}")?;
    }
    writeln!(writer, "  ABI Version:                       {}", elf.ident.abi_version)?;
    let file_type = file_type_name(elf.header.file_type);
    if file_type == "unknown" {
        writeln!(
            writer,
            "  Type:                              unknown (0x{:x})",
            elf.header.file_type
        )?;
    } else {
        writeln!(writer, "  Type:                              {file_type}")?;
    }
    let machine = machine_name(elf.header.machine);
    if machine == "unknown" {
        writeln!(
            writer,
            "  Machine:                           unknown (0x{:x})",
            elf.header.machine
        )?;
    } else {
        writeln!(writer, "  Machine:                           {machine}")?;
    }
    writeln!(writer, "  Version:                           0x{:x}", elf.header.version)?;
    writeln!(writer, "  Entry point address:               0x{:x}", elf.header.entry)?;
    writeln!(
        writer,
        "  Start of program headers:          {} (bytes into file)",
        elf.header.program_header_offset
    )?;
    writeln!(
        writer,
        "  Start of section headers:          {} (bytes into file)",
        elf.header.section_header_offset
    )?;
    writeln!(writer, "  Flags:                             0x{:x}", elf.header.flags)?;
    writeln!(writer, "  Size of this header:               {} (bytes)", elf.header.header_size)?;
    writeln!(
        writer,
        "  Size of program headers:           {} (bytes)",
        elf.header.program_header_entry_size
    )?;
    writeln!(writer, "  Number of program headers:         {}", elf.header.program_header_count)?;
    writeln!(
        writer,
        "  Size of section headers:           {} (bytes)",
        elf.header.section_header_entry_size
    )?;
    writeln!(writer, "  Number of section headers:         {}", elf.header.section_header_count)?;
    writeln!(
        writer,
        "  Section header string table index: {}",
        elf.header.section_name_string_index
    )
}

fn write_program_headers<W: Write>(writer: &mut W, elf: &ElfFile) -> io::Result<()> {
    writeln!(writer)?;
    writeln!(writer, "Program Headers:")?;
    if elf.ident.class64 {
        writeln!(
            writer,
            "  Type           Offset   VirtAddr           PhysAddr           FileSiz  MemSiz   Flg Align"
        )?;
    } else {
        writeln!(
            writer,
            "  Type           Offset   VirtAddr   PhysAddr   FileSiz  MemSiz   Flg Align"
        )?;
    }
    for header in &elf.program_headers {
        let type_name = program_header_type_name(header.segment_type);
        let flags = program_header_flags(header.flags);
        if elf.ident.class64 {
            writeln!(
                writer,
                "  {type_name:<14} 0x{:06x} 0x{:016x} 0x{:016x} 0x{:06x} 0x{:06x} {flags} 0x{:x}",
                header.offset,
                header.virtual_address,
                header.physical_address,
                header.file_size,
                header.memory_size,
                header.align
            )?;
        } else {
            writeln!(
                writer,
                "  {type_name:<14} 0x{:06x} 0x{:08x} 0x{:08x} 0x{:06x} 0x{:06x} {flags} 0x{:x}",
                header.offset,
                header.virtual_address,
                header.physical_address,
                header.file_size,
                header.memory_size,
                header.align
            )?;
        }
    }
    Ok(())
}

fn write_section_headers<W: Write>(writer: &mut W, elf: &ElfFile) -> io::Result<()> {
    writeln!(writer)?;
    writeln!(writer, "Section Headers:")?;
    if elf.ident.class64 {
        writeln!(
            writer,
            "  [Nr] Name              Type            Address          Off    Size   ES Flg Lk Inf Al"
        )?;
    } else {
        writeln!(
            writer,
            "  [Nr] Name              Type            Addr     Off    Size   ES Flg Lk Inf Al"
        )?;
    }
    for (index, header) in elf.section_headers.iter().enumerate() {
        let name = if header.name.is_empty() { String::new() } else { header.name.clone() };
        let display_name = if name.len() > 17 { format!("{}...", &name[..14]) } else { name };
        let type_name = section_type_name(header.section_type);
        let flags = section_flag_text(header.flags);
        if elf.ident.class64 {
            writeln!(
                writer,
                "  [{index:2}] {display_name:<17} {type_name:<15} {:016x} {:06x} {:06x} {:02x} {flags:>3} {:>2} {:>3} {:>2}",
                header.address,
                header.offset,
                header.size,
                header.entry_size,
                header.link,
                header.info,
                header.address_align
            )?;
        } else {
            writeln!(
                writer,
                "  [{index:2}] {display_name:<17} {type_name:<15} {:08x} {:06x} {:06x} {:02x} {flags:>3} {:>2} {:>3} {:>2}",
                header.address,
                header.offset,
                header.size,
                header.entry_size,
                header.link,
                header.info,
                header.address_align
            )?;
        }
    }
    writeln!(
        writer,
        "Key to Flags: W (write), A (alloc), X (execute), M (merge), S (strings), I (info), T (tls)"
    )
}

fn write_elf<W: Write>(
    mut writer: W,
    path: &str,
    elf: &ElfFile,
    options: Options,
) -> io::Result<()> {
    writeln!(writer, "{path}:")?;
    if options.header {
        write_header(&mut writer, elf)?;
    }
    if options.program_headers {
        write_program_headers(&mut writer, elf)?;
    }
    if options.section_headers {
        write_section_headers(&mut writer, elf)?;
    }
    Ok(())
}

/// Parses operands after `argv[0]`. With no `-h`/`-l`/`-S`, all sections are printed.
fn parse_args(args: &[String]) -> Result<(Options, Vec<&str>), String> {
    let mut selected = Options { header: false, program_headers: false, section_headers: false };
    let mut saw_selector = false;
    let mut operands = Vec::new();
    let mut options_ended = false;
    for arg in args {
        if !options_ended && arg == "--" {
            options_ended = true;
            continue;
        }
        if !options_ended && arg.starts_with('-') && arg != "-" {
            let flags: Vec<char> = arg.chars().skip(1).collect();
            if flags.is_empty() {
                return Err(format!("elfdump: invalid option -- '-'\n{USAGE}"));
            }
            for flag in flags {
                match flag {
                    'a' => {
                        selected = Options::default();
                        saw_selector = true;
                    }
                    'h' => {
                        selected.header = true;
                        saw_selector = true;
                    }
                    'l' => {
                        selected.program_headers = true;
                        saw_selector = true;
                    }
                    'S' => {
                        selected.section_headers = true;
                        saw_selector = true;
                    }
                    _ => return Err(format!("elfdump: invalid option -- '{flag}'\n{USAGE}")),
                }
            }
            continue;
        }
        operands.push(arg.as_str());
    }
    if operands.is_empty() {
        return Err(format!("elfdump: missing file operand\n{USAGE}"));
    }
    let options = if saw_selector { selected } else { Options::default() };
    Ok((options, operands))
}

fn run(args: &[String]) -> i32 {
    let (options, operands) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            return 1;
        }
    };

    let mut stdout = io::stdout().lock();
    let mut status = 0;
    for path in operands {
        match load_elf(path) {
            Ok(elf) => {
                if let Err(err) = write_elf(&mut stdout, path, &elf, options) {
                    let _ = writeln!(io::stderr(), "elfdump: {path}: {err}");
                    status = 1;
                }
            }
            Err(message) => {
                let _ = writeln!(io::stderr(), "elfdump: {path}: {message}");
                status = 1;
            }
        }
    }
    status
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if run(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_u16(buffer: &mut Vec<u8>, value: u16) {
        buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(buffer: &mut Vec<u8>, value: u32) {
        buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u64(buffer: &mut Vec<u8>, value: u64) {
        buffer.extend_from_slice(&value.to_le_bytes());
    }

    fn minimal_elf64() -> Vec<u8> {
        let string_table = b"\0.text\0.shstrtab\0";
        let program_header_offset = HEADER_SIZE_64 as u64;
        let text_offset = 0x200;
        let string_table_offset = 0x210;
        let section_header_offset = 0x230;

        let mut image = vec![0u8; 0x2f0];
        image[0..4].copy_from_slice(ELF_MAGIC);
        image[EI_CLASS] = ELFCLASS64;
        image[EI_DATA] = ELFDATA2LSB;
        image[EI_VERSION] = EV_CURRENT;

        let mut header = Vec::new();
        header.extend_from_slice(&image[..IDENT_SIZE]);
        push_u16(&mut header, 3); // ET_DYN
        push_u16(&mut header, 183); // EM_AARCH64
        push_u32(&mut header, 1);
        push_u64(&mut header, 0x200);
        push_u64(&mut header, program_header_offset);
        push_u64(&mut header, section_header_offset);
        push_u32(&mut header, 0);
        push_u16(&mut header, HEADER_SIZE_64);
        push_u16(&mut header, PROGRAM_HEADER_SIZE_64);
        push_u16(&mut header, 1);
        push_u16(&mut header, SECTION_HEADER_SIZE_64);
        push_u16(&mut header, 3);
        push_u16(&mut header, 2);
        image[..HEADER_SIZE_64 as usize].copy_from_slice(&header);

        let mut program_header = Vec::new();
        push_u32(&mut program_header, 1); // PT_LOAD
        push_u32(&mut program_header, 5); // r-x
        push_u64(&mut program_header, 0);
        push_u64(&mut program_header, 0);
        push_u64(&mut program_header, 0);
        push_u64(&mut program_header, 0x220);
        push_u64(&mut program_header, 0x220);
        push_u64(&mut program_header, 0x10000);
        image[program_header_offset as usize
            ..program_header_offset as usize + PROGRAM_HEADER_SIZE_64 as usize]
            .copy_from_slice(&program_header);

        image[text_offset as usize] = 0xd6;
        image[text_offset as usize + 1] = 0x5f;
        image[text_offset as usize + 2] = 0x03;
        image[text_offset as usize + 3] = 0xc0;
        image[string_table_offset as usize..string_table_offset as usize + string_table.len()]
            .copy_from_slice(string_table);

        let mut section_headers = Vec::new();
        section_headers.extend_from_slice(&[0u8; SECTION_HEADER_SIZE_64 as usize]);

        let mut text_header = Vec::new();
        push_u32(&mut text_header, 1); // ".text"
        push_u32(&mut text_header, 1); // SHT_PROGBITS
        push_u64(&mut text_header, 0x6); // AX
        push_u64(&mut text_header, 0x200);
        push_u64(&mut text_header, text_offset);
        push_u64(&mut text_header, 4);
        push_u32(&mut text_header, 0);
        push_u32(&mut text_header, 0);
        push_u64(&mut text_header, 4);
        push_u64(&mut text_header, 0);
        section_headers.extend_from_slice(&text_header);

        let mut string_header = Vec::new();
        push_u32(&mut string_header, 7); // ".shstrtab"
        push_u32(&mut string_header, 3); // SHT_STRTAB
        push_u64(&mut string_header, 0);
        push_u64(&mut string_header, 0);
        push_u64(&mut string_header, string_table_offset);
        push_u64(&mut string_header, string_table.len() as u64);
        push_u32(&mut string_header, 0);
        push_u32(&mut string_header, 0);
        push_u64(&mut string_header, 1);
        push_u64(&mut string_header, 0);
        section_headers.extend_from_slice(&string_header);

        let start = section_header_offset as usize;
        image[start..start + section_headers.len()].copy_from_slice(&section_headers);
        image
    }

    #[test]
    fn parse_args_requires_file() {
        let args: [String; 0] = [];
        assert!(parse_args(&args).unwrap_err().contains("missing file operand"));
    }

    #[test]
    fn parse_args_defaults_to_all() {
        let args = ["a.out".to_string()];
        let (options, operands) = parse_args(&args).unwrap();
        assert_eq!(options, Options::default());
        assert_eq!(operands, ["a.out"]);
    }

    #[test]
    fn parse_args_selectors() {
        let args = ["-hS".to_string(), "lib.so".to_string()];
        let (options, operands) = parse_args(&args).unwrap();
        assert!(options.header);
        assert!(!options.program_headers);
        assert!(options.section_headers);
        assert_eq!(operands, ["lib.so"]);
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let args = ["-z".to_string(), "a.out".to_string()];
        assert!(parse_args(&args).unwrap_err().contains("invalid option"));
    }

    #[test]
    fn parse_ident_rejects_non_elf() {
        let mut bytes = [0u8; IDENT_SIZE];
        bytes[..4].copy_from_slice(b"NOTL");
        assert!(parse_ident(&bytes).unwrap_err().contains("not an ELF file"));
    }

    #[test]
    fn parse_and_format_minimal_elf64() {
        let image = minimal_elf64();
        let ident_bytes: [u8; IDENT_SIZE] = image[..IDENT_SIZE].try_into().unwrap();
        let ident = parse_ident(&ident_bytes).unwrap();
        let header = parse_file_header(ident, &image[..HEADER_SIZE_64 as usize]).unwrap();
        assert!(ident.class64);
        assert!(ident.little_endian);
        assert_eq!(header.file_type, 3);
        assert_eq!(header.machine, 183);
        assert_eq!(header.program_header_count, 1);
        assert_eq!(header.section_header_count, 3);

        let program = parse_program_header(
            ident,
            &image[header.program_header_offset as usize
                ..header.program_header_offset as usize + PROGRAM_HEADER_SIZE_64 as usize],
        )
        .unwrap();
        assert_eq!(program.segment_type, 1);
        assert_eq!(program_header_flags(program.flags), "r-x");

        let string_table = b"\0.text\0.shstrtab\0";
        assert_eq!(string_from_table(string_table, 1), ".text");
        assert_eq!(string_from_table(string_table, 7), ".shstrtab");

        let elf = ElfFile {
            ident_bytes,
            ident,
            header,
            program_headers: vec![program],
            section_headers: vec![
                SectionHeader {
                    name: String::new(),
                    section_type: 0,
                    flags: 0,
                    address: 0,
                    offset: 0,
                    size: 0,
                    link: 0,
                    info: 0,
                    address_align: 0,
                    entry_size: 0,
                },
                SectionHeader {
                    name: ".text".to_string(),
                    section_type: 1,
                    flags: 6,
                    address: 0x200,
                    offset: 0x200,
                    size: 4,
                    link: 0,
                    info: 0,
                    address_align: 4,
                    entry_size: 0,
                },
            ],
        };
        let mut output = Vec::new();
        write_elf(
            &mut output,
            "test.elf",
            &elf,
            Options { header: true, program_headers: true, section_headers: false },
        )
        .unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Class:                             ELF64"));
        assert!(text.contains("Machine:                           AArch64"));
        assert!(text.contains("Type:                              DYN (Shared object file)"));
        assert!(text.contains("LOAD"));
        assert!(text.contains("r-x"));
    }

    #[test]
    fn program_header_flags_bits() {
        assert_eq!(program_header_flags(0), "---");
        assert_eq!(program_header_flags(4), "r--");
        assert_eq!(program_header_flags(6), "rw-");
        assert_eq!(program_header_flags(7), "rwx");
    }

    #[test]
    fn load_elf_round_trip() {
        let image = minimal_elf64();
        let dir = std::env::temp_dir().join(format!(
            "secular-elfdump-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.out");
        std::fs::write(&path, &image).unwrap();
        let elf = load_elf(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(elf.header.machine, 183);
        assert_eq!(elf.program_headers.len(), 1);
        assert_eq!(elf.section_headers.len(), 3);
        assert_eq!(elf.section_headers[1].name, ".text");
        assert_eq!(elf.section_headers[2].name, ".shstrtab");
    }
}
