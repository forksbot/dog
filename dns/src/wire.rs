//! Parsing the DNS wire protocol.

pub(crate) use std::io::Cursor;
pub(crate) use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};

use std::io;
use log::{error, info, debug};

use crate::record::{Record, OPT};
use crate::strings::{ReadLabels, WriteLabels};
use crate::types::*;


impl Request {

    /// Converts this request to a vector of bytes.
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(32);

        bytes.write_u16::<BigEndian>(self.transaction_id)?;
        bytes.write_u16::<BigEndian>(self.flags.to_u16())?;

        bytes.write_u16::<BigEndian>(self.queries.len() as u16)?;
        bytes.write_u16::<BigEndian>(0)?;  // usually answers
        bytes.write_u16::<BigEndian>(0)?;  // usually authority RRs
        bytes.write_u16::<BigEndian>(if self.additional.is_some() { 1 } else { 0 })?;  // additional RRs

        for query in &self.queries {
            bytes.write_labels(&query.qname)?;
            bytes.write_u16::<BigEndian>(query.qtype)?;
            bytes.write_u16::<BigEndian>(query.qclass.to_u16())?;
        }

        if let Some(opt) = &self.additional {
            bytes.write_u8(0)?;  // usually a name
            bytes.write_u16::<BigEndian>(OPT::RR_TYPE)?;
            bytes.extend(opt.to_bytes()?);
        }

        Ok(bytes)
    }

    /// Returns the OPT record to be sent as part of requests.
    pub fn additional_record() -> OPT {
        OPT {
            udp_payload_size: 512,
            higher_bits: 0,
            edns0_version: 0,
            flags: 0,
            data: Vec::new(),
        }
    }
}


impl Response {

    /// Reads bytes off of the given slice, parsing them into a response.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WireError> {
        debug!("Parsing bytes -> {:?}", bytes);

        let mut c = Cursor::new(bytes);
        let transaction_id = c.read_u16::<BigEndian>()?;
        let flags = Flags::from_u16(c.read_u16::<BigEndian>()?);
        debug!("Read flags: {:#?}", flags);

        let query_count      = c.read_u16::<BigEndian>()?;
        let answer_count     = c.read_u16::<BigEndian>()?;
        let authority_count  = c.read_u16::<BigEndian>()?;
        let additional_count = c.read_u16::<BigEndian>()?;

        let mut queries = Vec::new();
        debug!("Reading {}x query from response", query_count);
        for _ in 0 .. query_count {
            let qname = c.read_labels()?;
            queries.push(Query::from_bytes(qname, &mut c)?);
        }

        let mut answers = Vec::new();
        debug!("Reading {}x answer from response", answer_count);
        for _ in 0 .. answer_count {
            let qname = c.read_labels()?;
            answers.push(Answer::from_bytes(qname, &mut c)?);
        }

        let mut authorities = Vec::new();
        debug!("Reading {}x authority from response", authority_count);
        for _ in 0 .. authority_count {
            let qname = c.read_labels()?;
            authorities.push(Answer::from_bytes(qname, &mut c)?);
        }

        let mut additionals = Vec::new();
        debug!("Reading {}x additional answer from response", additional_count);
        for _ in 0 .. additional_count {
            let qname = c.read_labels()?;
            additionals.push(Answer::from_bytes(qname, &mut c)?);
        }

        Ok(Response { transaction_id, flags, queries, answers, authorities, additionals })
    }
}


impl Query {

    /// Reads bytes from the given cursor, and parses them into a query with
    /// the given domain name.
    fn from_bytes(qname: String, c: &mut Cursor<&[u8]>) -> Result<Self, WireError> {
        let qtype = c.read_u16::<BigEndian>()?;
        let qclass = QClass::from_u16(c.read_u16::<BigEndian>()?);

        Ok(Query { qtype, qclass, qname })
    }
}


impl Answer {

    /// Reads bytes from the given cursor, and parses them into an answer with
    /// the given domain name.
    fn from_bytes(qname: String, c: &mut Cursor<&[u8]>) -> Result<Self, WireError> {
        let qtype = c.read_u16::<BigEndian>()?;
        if qtype == OPT::RR_TYPE {
            let opt = OPT::read(c)?;
            Ok(Answer::Pseudo { qname, opt })
        }
        else {
            let qclass = QClass::from_u16(c.read_u16::<BigEndian>()?);
            let ttl = c.read_u32::<BigEndian>()?;

            let len = c.read_u16::<BigEndian>()?;
            let record = Record::from_bytes(qtype, len, c)?;

            Ok(Answer::Standard { qclass, qname, record, ttl })
        }

    }
}


impl Record {

    /// Reads at most `len` bytes from the given curser, and parses them into
    /// a record structure depending on the type number, which has already been read.
    fn from_bytes(qtype: TypeInt, len: u16, c: &mut Cursor<&[u8]>) -> Result<Record, WireError> {
        use crate::record::*;

        macro_rules! try_record {
            ($record:tt) => {
                if $record::RR_TYPE == qtype {
                    info!("Deciphering {} record (type {}, len {})", $record::NAME, qtype, len);
                    return Wire::read(len, c).map(Record::$record)
                }
            }
        }

        // Try all the records, one type at a time, returning early if the
        // type number matches.
        try_record!(A);
        try_record!(AAAA);
        try_record!(CAA);
        try_record!(CNAME);
        try_record!(MX);
        try_record!(NS);
        // OPT is handled separately
        try_record!(PTR);
        try_record!(SOA);
        try_record!(SRV);
        try_record!(TXT);

        // Otherwise, collect the bytes into a vector and return an unknown
        // record type.
        let mut bytes = Vec::new();
        for _ in 0 .. len {
            bytes.push(c.read_u8()?);
        }

        let type_number = UnknownQtype::from(qtype);
        Ok(Record::Other { type_number, bytes })
    }
}


impl QClass {
    fn from_u16(uu: u16) -> Self {
        match uu {
            0x0001 => QClass::IN,
            0x0003 => QClass::CH,
            0x0004 => QClass::HS,
                 _ => QClass::Other(uu),
        }
    }

    fn to_u16(self) -> u16 {
        match self {
            QClass::IN        => 0x0001,
            QClass::CH        => 0x0003,
            QClass::HS        => 0x0004,
            QClass::Other(uu) => uu,
        }
    }
}


/// Determines the record type number to signify a record with the given name.
pub fn find_qtype_number(record_type: &str) -> Option<TypeInt> {
    use crate::record::*;

    macro_rules! try_record {
        ($record:tt) => {
            if $record::NAME == record_type {
                return Some($record::RR_TYPE);
            }
        }
    }

    try_record!(A);
    try_record!(AAAA);
    try_record!(CAA);
    try_record!(CNAME);
    try_record!(MX);
    try_record!(NS);
    // OPT is elsewhere
    try_record!(PTR);
    try_record!(SOA);
    try_record!(SRV);
    try_record!(TXT);

    None
}


impl Flags {

    /// The set of flags that represents a query packet.
    pub fn query() -> Self {
        Self::from_u16(0b_0000_0001_0000_0000)
    }

    /// Converts the flags into a two-byte number.
    pub fn to_u16(self) -> u16 {                 // 0123 4567 89AB CDEF
        let mut                          bits  = 0b_0000_0000_0000_0000;
        if self.response               { bits += 0b_1000_0000_0000_0000; }
        match self.opcode {
                                _ =>   { bits += 0b_0000_0000_0000_0000; }
        }
        if self.authoritative          { bits += 0b_0000_0100_0000_0000; }
        if self.truncated              { bits += 0b_0000_0010_0000_0000; }
        if self.recursion_desired      { bits += 0b_0000_0001_0000_0000; }
        if self.recursion_available    { bits += 0b_0000_0000_1000_0000; }
        // (the Z bit is reserved)               0b_0000_0000_0100_0000
        if self.authentic_data         { bits += 0b_0000_0000_0010_0000; }
        if self.checking_disabled      { bits += 0b_0000_0000_0001_0000; }

        bits
    }

    /// Extracts the flags from the given two-byte number.
    pub fn from_u16(bits: u16) -> Self {
        let has_bit = |bit| { bits & bit == bit };

        Flags {
            response:               has_bit(0b_1000_0000_0000_0000),
            opcode:                 0,
            authoritative:          has_bit(0b_0000_0100_0000_0000),
            truncated:              has_bit(0b_0000_0010_0000_0000),
            recursion_desired:      has_bit(0b_0000_0001_0000_0000),
            recursion_available:    has_bit(0b_0000_0000_1000_0000),
            authentic_data:         has_bit(0b_0000_0000_0010_0000),
            checking_disabled:      has_bit(0b_0000_0000_0001_0000),
            error_code:             ErrorCode::from_bits(bits & 0b_1111),
        }
    }
}


impl ErrorCode {

    /// Extracts the rcode from the last four bits of the flags field.
    fn from_bits(bits: u16) -> Option<Self> {
        match bits {
            0 => None,
            1 => Some(Self::FormatError),
            2 => Some(Self::ServerFailure),
            3 => Some(Self::NXDomain),
            4 => Some(Self::NotImplemented),
            5 => Some(Self::QueryRefused),
           16 => Some(Self::BadVersion),
            n => Some(Self::Other(n)),
        }
    }
}


/// Trait for decoding DNS record structures from bytes read over the wire.
pub trait Wire: Sized {

    /// This record’s type as a string, such as `"A"` or `"CNAME"`.
    const NAME: &'static str;

    /// The number signifying that a record is of this type.
    /// See <https://www.iana.org/assignments/dns-parameters/dns-parameters.xhtml#dns-parameters-4>
    const RR_TYPE: u16;

    /// Read at most `len` bytes from the given `Cursor`. This cursor travels
    /// throughout the complete data — by this point, we have read the entire
    /// response into a buffer.
    fn read(len: u16, c: &mut Cursor<&[u8]>) -> Result<Self, WireError>;
}


/// Helper macro to get the qtype number of a record type at compile-time.
///
/// # Examples
///
/// ```
/// use dns::{qtype, record::MX};
///
/// assert_eq!(15, qtype!(MX));
/// ```
#[macro_export]
macro_rules! qtype {
    ($type:ty) => {
        <$type as $crate::Wire>::RR_TYPE
    }
}


/// Something that can go wrong deciphering a record.
#[derive(PartialEq, Debug)]
pub enum WireError {

    /// There was an IO error reading from the cursor.
    /// Almost all the time, this means that the buffer was too short.
    IO,
    // (io::Error is not PartialEq so we don’t propagate it)

    /// When this record expected the data to be a certain size, but it was
    /// a different one.
    WrongLength {

        /// The expected size.
        expected: u16,

        /// The size that was actually received.
        got: u16,
    },

    /// When the data contained a string containing a cycle of pointers.
    /// Contains the vector of indexes that was being checked.
    TooMuchRecursion(Vec<u16>),

    /// When the data contained a string with a pointer to an index outside of
    /// the packet. Contains the invalid index.
    OutOfBounds(u16),
}

impl From<io::Error> for WireError {
    fn from(ioe: io::Error) -> Self {
        error!("IO error -> {:?}", ioe);
        WireError::IO
    }
}
