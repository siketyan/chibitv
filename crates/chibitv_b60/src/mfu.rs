use std::io::{Cursor, Read};

use byteorder::{BE, ReadBytesExt};

use crate::mmtp::MpuFragment;

#[derive(Clone, Debug)]
pub struct MfuTimedData {
    pub movie_fragment_sequence_number: u32,
    pub sample_number: u32,
    pub offset: u32,
    pub priority: u8,
    pub dependency_counter: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct MfuNonTimedData {
    pub item_id: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum MfuPayload {
    TimedAggregated(Vec<MfuTimedData>),
    Timed(MfuTimedData),
    Aggregated(Vec<MfuNonTimedData>),
    Default(MfuNonTimedData),
}

impl TryFrom<&MpuFragment> for MfuPayload {
    type Error = std::io::Error;

    fn try_from(value: &MpuFragment) -> std::result::Result<Self, Self::Error> {
        let mut reader = Cursor::new(&value.payload);
        let mut index = 0;

        if value.timed_flag {
            if !value.aggregation_flag {
                let movie_fragment_sequence_number = reader.read_u32::<BE>()?;
                let sample_number = reader.read_u32::<BE>()?;
                let offset = reader.read_u32::<BE>()?;
                let priority = reader.read_u8()?;
                let dependency_counter = reader.read_u8()?;

                let mut data = Vec::new();
                reader.read_to_end(&mut data)?;

                Ok(MfuPayload::Timed(MfuTimedData {
                    movie_fragment_sequence_number,
                    sample_number,
                    offset,
                    priority,
                    dependency_counter,
                    data,
                }))
            } else {
                let mut data = Vec::new();

                while index < value.payload.len() {
                    let data_unit_length = reader.read_u16::<BE>()?;
                    let remaining_len = value.payload.len() - index;
                    assert!(
                        usize::from(data_unit_length) <= remaining_len,
                        "insufficient buffer size: {data_unit_length} > {remaining_len}"
                    );

                    let movie_fragment_sequence_number = reader.read_u32::<BE>()?;
                    let sample_number = reader.read_u32::<BE>()?;
                    let offset = reader.read_u32::<BE>()?;
                    let priority = reader.read_u8()?;
                    let dependency_counter = reader.read_u8()?;

                    let buf_len = (data_unit_length - 14) as usize;
                    let mut buf = vec![0u8; buf_len];
                    reader.read_exact(&mut buf)?;

                    data.push(MfuTimedData {
                        movie_fragment_sequence_number,
                        sample_number,
                        offset,
                        priority,
                        dependency_counter,
                        data: buf,
                    });

                    index += usize::from(data_unit_length + 2);
                }

                Ok(MfuPayload::TimedAggregated(data))
            }
        } else if !value.aggregation_flag {
            let item_id = reader.read_u32::<BE>()?;

            let mut data = Vec::new();
            reader.read_to_end(&mut data)?;

            Ok(MfuPayload::Default(MfuNonTimedData { item_id, data }))
        } else {
            let mut data = Vec::new();

            while index < value.payload.len() {
                let data_unit_length = reader.read_u16::<BE>()?;
                let remaining_len = value.payload.len() - index;
                assert!(
                    usize::from(data_unit_length) <= remaining_len,
                    "insufficient buffer size: {data_unit_length} > {remaining_len}"
                );

                let item_id = reader.read_u32::<BE>()?;

                let mut buf = vec![0u8; (data_unit_length - 4) as usize];
                reader.read_exact(&mut buf)?;

                data.push(MfuNonTimedData { item_id, data: buf });

                index += usize::from(data_unit_length + 2);
            }

            Ok(MfuPayload::Aggregated(data))
        }
    }
}
