use std::str;

use chrono::{NaiveDate, NaiveTime};
use nom::branch::alt;
use nom::bytes::complete::{tag, take, take_until, take_while1};
use nom::character::complete::{char, digit1, one_of};
use nom::combinator::{all_consuming, cond, map, map_parser, map_res, opt, rest_len, value};
use nom::multi::many0;
use nom::number::complete::{double, float};
use nom::sequence::{preceded, terminated, tuple};
use nom::IResult;

use crate::{FixType, GnssType, Satellite, SentenceType};

pub struct NmeaSentence<'a> {
    pub talker_id: &'a [u8],
    pub message_id: &'a [u8],
    pub data: &'a [u8],
    pub checksum: u8,
}

impl<'a> NmeaSentence<'a> {
    pub fn calc_checksum(&self) -> u8 {
        checksum(
            self.talker_id
                .iter()
                .chain(self.message_id.iter())
                .chain(&[b','])
                .chain(self.data.iter()),
        )
    }
}

pub struct GsvData {
    pub gnss_type: GnssType,
    pub number_of_sentences: u16,
    pub sentence_num: u16,
    pub _sats_in_view: u16,
    pub sats_info: [Option<Satellite>; 4],
}

pub fn checksum<'a, I: Iterator<Item = &'a u8>>(bytes: I) -> u8 {
    bytes.fold(0, |c, x| c ^ *x)
}

fn parse_hex(data: &[u8]) -> std::result::Result<u8, &'static str> {
    u8::from_str_radix(unsafe { str::from_utf8_unchecked(data) }, 16)
        .map_err(|_| "Failed to parse checksum as hex number")
}

fn parse_checksum(i: &[u8]) -> IResult<&[u8], u8> {
    map_res(preceded(char('*'), take(2usize)), parse_hex)(i)
}

fn do_parse_nmea_sentence(i: &[u8]) -> IResult<&[u8], NmeaSentence> {
    let (i, talker_id) = preceded(char('$'), take(2usize))(i)?;
    let (i, message_id) = take(3usize)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, data) = take_until("*")(i)?;
    let (i, checksum) = parse_checksum(i)?;

    Ok((
        i,
        NmeaSentence {
            talker_id,
            message_id,
            data,
            checksum,
        },
    ))
}

pub fn parse_nmea_sentence(sentence: &[u8]) -> std::result::Result<NmeaSentence, String> {
    /*
     * From gpsd:
     * We've had reports that on the Garmin GPS-10 the device sometimes
     * (1:1000 or so) sends garbage packets that have a valid checksum
     * but are like 2 successive NMEA packets merged together in one
     * with some fields lost.  Usually these are much longer than the
     * legal limit for NMEA, so we can cope by just tossing out overlong
     * packets.  This may be a generic bug of all Garmin chipsets.
     * NMEA 3.01, Section 5.3 says the max sentence length shall be
     * 82 chars, including the leading $ and terminating \r\n.
     *
     * Some receivers (TN-200, GSW 2.3.2) emit oversized sentences.
     * The Trimble BX-960 receiver emits a 91-character GGA message.
     * The current hog champion is the Skytraq S2525F8 which emits
     * a 100-character PSTI message.
     */
    if sentence.len() > 102 {
        return Err("Too long message".to_string());
    }
    let res: NmeaSentence = do_parse_nmea_sentence(sentence)
        .map_err(|err| match err {
            nom::Err::Incomplete(_) => "Incomplete nmea sentence".to_string(),
            nom::Err::Error((_, kind)) | nom::Err::Failure((_, kind)) => {
                kind.description().to_string()
            }
        })?
        .1;
    Ok(res)
}

fn parse_num<I: std::str::FromStr>(data: &[u8]) -> std::result::Result<I, &'static str> {
    //    println!("parse num {}", unsafe { str::from_utf8_unchecked(data) });
    str::parse::<I>(unsafe { str::from_utf8_unchecked(data) }).map_err(|_| "parse of number failed")
}
fn number<T: std::str::FromStr>(i: &[u8]) -> IResult<&[u8], T> {
    map_res(digit1, parse_num)(i)
}

fn parse_gsv_sat_info(i: &[u8]) -> IResult<&[u8], Satellite> {
    let (i, prn) = number::<u32>(i)?;
    let (i, _) = char(',')(i)?;
    let (i, elevation) = opt(number::<i32>)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, azimuth) = opt(number::<i32>)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, snr) = opt(number::<i32>)(i)?;
    let (i, _) = cond(rest_len(i)?.1 > 0, char(','))(i)?;
    Ok((
        i,
        Satellite {
            gnss_type: GnssType::Galileo,
            prn,
            elevation: elevation.map(|v| v as f32),
            azimuth: azimuth.map(|v| v as f32),
            snr: snr.map(|v| v as f32),
        },
    ))
}

fn do_parse_gsv(i: &[u8]) -> IResult<&[u8], GsvData> {
    let (i, number_of_sentences) = number::<u16>(i)?;
    let (i, _) = char(',')(i)?;
    let (i, sentence_num) = number::<u16>(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _sats_in_view) = number::<u16>(i)?;
    let (i, _) = char(',')(i)?;
    let (i, sat0) = opt(parse_gsv_sat_info)(i)?;
    let (i, sat1) = opt(parse_gsv_sat_info)(i)?;
    let (i, sat2) = opt(parse_gsv_sat_info)(i)?;
    let (i, sat3) = opt(parse_gsv_sat_info)(i)?;
    Ok((
        i,
        GsvData {
            gnss_type: GnssType::Galileo,
            number_of_sentences,
            sentence_num,
            _sats_in_view,
            sats_info: [sat0, sat1, sat2, sat3],
        },
    ))
}

/// Parsin one GSV sentence
/// from gpsd/driver_nmea0183.c:
/// $IDGSV,2,1,08,01,40,083,46,02,17,308,41,12,07,344,39,14,22,228,45*75
/// 2           Number of sentences for full data
/// 1           Sentence 1 of 2
/// 08          Total number of satellites in view
/// 01          Satellite PRN number
/// 40          Elevation, degrees
/// 083         Azimuth, degrees
/// 46          Signal-to-noise ratio in decibels
/// <repeat for up to 4 satellites per sentence>
///
/// Can occur with talker IDs:
///   BD (Beidou),
///   GA (Galileo),
///   GB (Beidou),
///   GL (GLONASS),
///   GN (GLONASS, any combination GNSS),
///   GP (GPS, SBAS, QZSS),
///   QZ (QZSS).
///
/// GL may be (incorrectly) used when GSVs are mixed containing
/// GLONASS, GN may be (incorrectly) used when GSVs contain GLONASS
/// only.  Usage is inconsistent.
pub fn parse_gsv(sentence: &NmeaSentence) -> Result<GsvData, String> {
    if sentence.message_id != b"GSV" {
        return Err("GSV sentence not starts with $..GSV".into());
    }
    let gnss_type = match sentence.talker_id {
        b"GP" => GnssType::Gps,
        b"GA" => GnssType::Galileo,
        b"GL" | b"GN" => GnssType::Glonass,
        _ => return Err("Unknown GNSS type in GSV sentence".into()),
    };
    //    println!("parse: '{}'", str::from_utf8(sentence.data).unwrap());
    let mut res: GsvData = do_parse_gsv(sentence.data)
        .map_err(|err| match err {
            nom::Err::Incomplete(_) => "Incomplete nmea sentence".to_string(),
            nom::Err::Error((_, kind)) | nom::Err::Failure((_, kind)) => {
                kind.description().to_string()
            }
        })?
        .1;
    res.gnss_type = gnss_type.clone();
    for sat in &mut res.sats_info {
        if let Some(v) = (*sat).as_mut() {
            v.gnss_type = gnss_type.clone();
        }
    }
    Ok(res)
}

#[derive(Debug, PartialEq)]
pub struct GgaData {
    pub fix_time: Option<NaiveTime>,
    pub fix_type: Option<FixType>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub fix_satellites: Option<u32>,
    pub hdop: Option<f32>,
    pub altitude: Option<f32>,
    pub geoid_height: Option<f32>,
}

fn parse_float_num<T: str::FromStr>(input: &[u8]) -> std::result::Result<T, &'static str> {
    let s = str::from_utf8(input).map_err(|_| "invalid float number")?;
    str::parse::<T>(s).map_err(|_| "parse of float number failed")
}

fn parse_hms(i: &[u8]) -> IResult<&[u8], NaiveTime> {
    map_res(
        tuple((
            map_res(take(2usize), parse_num::<u32>),
            map_res(take(2usize), parse_num::<u32>),
            map_parser(take_until(","), double),
        )),
        |(hour, minutes, sec)| -> std::result::Result<NaiveTime, &'static str> {
            if sec.is_sign_negative() {
                return Err("Invalid time: second is negative");
            }
            if hour >= 24 {
                return Err("Invalid time: hour >= 24");
            }
            if minutes >= 60 {
                return Err("Invalid time: min >= 60");
            }
            Ok(NaiveTime::from_hms_nano(
                hour,
                minutes,
                sec.trunc() as u32,
                (sec.fract() * 1_000_000_000f64).round() as u32,
            ))
        },
    )(i)
}

fn do_parse_lat_lon(i: &[u8]) -> IResult<&[u8], (f64, f64)> {
    let (i, lat_deg) = map_res(take(2usize), parse_num::<u8>)(i)?;
    let (i, lat_min) = double(i)?;
    let (i, _) = char(',')(i)?;
    let (i, lat_dir) = one_of("NS")(i)?;
    let (i, _) = char(',')(i)?;
    let (i, lon_deg) = map_res(take(3usize), parse_num::<u8>)(i)?;
    let (i, lon_min) = double(i)?;
    let (i, _) = char(',')(i)?;
    let (i, lon_dir) = one_of("EW")(i)?;

    let mut lat = f64::from(lat_deg) + lat_min / 60.;
    if lat_dir == 'S' {
        lat = -lat;
    }
    let mut lon = f64::from(lon_deg) + lon_min / 60.;
    if lon_dir == 'W' {
        lon = -lon;
    }

    Ok((i, (lat, lon)))
}

fn parse_lat_lon(i: &[u8]) -> IResult<&[u8], Option<(f64, f64)>> {
    alt((map(tag(",,,"), |_| None), map(do_parse_lat_lon, Some)))(i)
}

fn do_parse_gga(i: &[u8]) -> IResult<&[u8], GgaData> {
    let (i, fix_time) = opt(parse_hms)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, lat_lon) = parse_lat_lon(i)?;
    let (i, _) = char(',')(i)?;
    let (i, fix_quality) = one_of("012345678")(i)?;
    let (i, _) = char(',')(i)?;
    let (i, fix_satellites) = opt(number::<u32>)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, hdop) = opt(float)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, altitude) = opt(map_res(take_until(","), parse_float_num::<f32>))(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _) = opt(char('M'))(i)?;
    let (i, _) = char(',')(i)?;
    let (i, geoid_height) = opt(map_res(take_until(","), parse_float_num::<f32>))(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _) = opt(char('M'))(i)?;

    Ok((
        i,
        GgaData {
            fix_time,
            fix_type: Some(FixType::from(fix_quality)),
            latitude: lat_lon.map(|v| v.0),
            longitude: lat_lon.map(|v| v.1),
            fix_satellites,
            hdop,
            altitude,
            geoid_height,
        },
    ))
}

/// Parse GGA message
/// from gpsd/driver_nmea0183.c
/// GGA,123519,4807.038,N,01131.324,E,1,08,0.9,545.4,M,46.9,M, , *42
/// 1     123519       Fix taken at 12:35:19 UTC
/// 2,3   4807.038,N   Latitude 48 deg 07.038' N
/// 4,5   01131.324,E  Longitude 11 deg 31.324' E
/// 6         1            Fix quality: 0 = invalid, 1 = GPS, 2 = DGPS,
/// 3=PPS (Precise Position Service),
/// 4=RTK (Real Time Kinematic) with fixed integers,
/// 5=Float RTK, 6=Estimated, 7=Manual, 8=Simulator
/// 7     08       Number of satellites being tracked
/// 8     0.9              Horizontal dilution of position
/// 9,10  545.4,M      Altitude, Metres above mean sea level
/// 11,12 46.9,M       Height of geoid (mean sea level) above WGS84
/// ellipsoid, in Meters
/// (empty field) time in seconds since last DGPS update
/// (empty field) DGPS station ID number (0000-1023)
pub fn parse_gga(sentence: &NmeaSentence) -> Result<GgaData, String> {
    if sentence.message_id != b"GGA" {
        return Err("GGA sentence not starts with $..GGA".into());
    }
    let res: GgaData = do_parse_gga(sentence.data)
        .map_err(|err| match err {
            nom::Err::Incomplete(_) => "Incomplete nmea sentence".to_string(),
            nom::Err::Error((_, kind)) | nom::Err::Failure((_, kind)) => {
                kind.description().to_string()
            }
        })?
        .1;
    Ok(res)
}

#[derive(Debug, PartialEq)]
pub enum RmcStatusOfFix {
    Autonomous,
    Differential,
    Invalid,
}

#[derive(Debug, PartialEq)]
pub struct RmcData {
    pub fix_time: Option<NaiveTime>,
    pub fix_date: Option<NaiveDate>,
    pub status_of_fix: Option<RmcStatusOfFix>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub speed_over_ground: Option<f32>,
    pub true_course: Option<f32>,
}

fn parse_date(i: &[u8]) -> IResult<&[u8], NaiveDate> {
    map_res(
        tuple((
            map_res(take(2usize), parse_num::<u8>),
            map_res(take(2usize), parse_num::<u8>),
            map_res(take(2usize), parse_num::<u8>),
        )),
        |data| -> Result<NaiveDate, &'static str> {
            let (day, month, year) = (u32::from(data.0), u32::from(data.1), i32::from(data.2));
            if month < 1 || month > 12 {
                return Err("Invalid month < 1 or > 12");
            }
            if day < 1 || day > 31 {
                return Err("Invalid day < 1 or > 31");
            }
            Ok(NaiveDate::from_ymd(year, month, day))
        },
    )(i)
}

fn do_parse_rmc(i: &[u8]) -> IResult<&[u8], RmcData> {
    map_res(
        tuple((
            terminated(opt(parse_hms), char(',')),
            terminated(one_of("ADV"), char(',')),
            terminated(parse_lat_lon, char(',')),
            terminated(
                opt(float),
                char(','),
            ),
            terminated(
                opt(float),
                char(','),
            ),
            terminated(opt(parse_date), char(',')),
        )),
        |(fix_time, status_of_fix, lat_lon, speed_over_ground, true_course, fix_date)|
                -> Result<RmcData, &'static str> {
            Ok(RmcData {
                fix_time,
                fix_date,
                status_of_fix: Some(match status_of_fix {
                    'A' => RmcStatusOfFix::Autonomous,
                    'D' => RmcStatusOfFix::Differential,
                    'V' => RmcStatusOfFix::Invalid,
                    _ => return Err("do_parse_rmc failed: not A|D|V status of fix"),
                }),
                lat: lat_lon.map(|v| v.0),
                lon: lat_lon.map(|v| v.1),
                speed_over_ground,
                true_course,
            })
        },
    )(i)
}

/// Parse RMC message
/// From gpsd:
/// RMC,225446.33,A,4916.45,N,12311.12,W,000.5,054.7,191194,020.3,E,A*68
/// 1     225446.33    Time of fix 22:54:46 UTC
/// 2     A          Status of Fix: A = Autonomous, valid;
/// D = Differential, valid; V = invalid
/// 3,4   4916.45,N    Latitude 49 deg. 16.45 min North
/// 5,6   12311.12,W   Longitude 123 deg. 11.12 min West
/// 7     000.5      Speed over ground, Knots
/// 8     054.7      Course Made Good, True north
/// 9     181194       Date of fix  18 November 1994
/// 10,11 020.3,E      Magnetic variation 20.3 deg East
/// 12    A      FAA mode indicator (NMEA 2.3 and later)
/// A=autonomous, D=differential, E=Estimated,
/// N=not valid, S=Simulator, M=Manual input mode
/// *68        mandatory nmea_checksum
///
/// SiRF chipsets don't return either Mode Indicator or magnetic variation.
pub fn parse_rmc(sentence: &NmeaSentence) -> Result<RmcData, String> {
    if sentence.message_id != b"RMC" {
        return Err("RMC message should starts with $..RMC".into());
    }
    do_parse_rmc(sentence.data)
        .map(|(_, data)| data)
        .map_err(|err| match err {
            nom::Err::Incomplete(_) => "Incomplete nmea sentence".to_string(),
            nom::Err::Error((_, kind)) | nom::Err::Failure((_, kind)) => {
                kind.description().to_string()
            }
        })
}

#[derive(PartialEq, Debug)]
pub enum GsaMode1 {
    Manual,
    Automatic,
}

#[derive(Debug, PartialEq)]
pub enum GsaMode2 {
    NoFix,
    Fix2D,
    Fix3D,
}

#[derive(Debug, PartialEq)]
pub struct GsaData {
    pub mode1: GsaMode1,
    pub mode2: GsaMode2,
    pub fix_sats_prn: Vec<u32>,
    pub pdop: Option<f32>,
    pub hdop: Option<f32>,
    pub vdop: Option<f32>,
}

fn gsa_prn_fields_parse(i: &[u8]) -> IResult<&[u8], Vec<Option<u32>>> {
    many0(terminated(opt(number::<u32>), char(',')))(i)
}

type GsaTail = (Vec<Option<u32>>, Option<f32>, Option<f32>, Option<f32>);

fn do_parse_gsa_tail(i: &[u8]) -> IResult<&[u8], GsaTail> {
    let (i, prns) = gsa_prn_fields_parse(i)?;
    let (i, pdop) = float(i)?;
    let (i, _) = char(',')(i)?;
    let (i, hdop) = float(i)?;
    let (i, _) = char(',')(i)?;
    let (i, vdop) = float(i)?;
    Ok((i, (prns, Some(pdop), Some(hdop), Some(vdop))))
}

fn is_comma(x: u8) -> bool {
    x == b','
}

fn do_parse_empty_gsa_tail(i: &[u8]) -> IResult<&[u8], GsaTail> {
    value(
        (Vec::new(), None, None, None),
        all_consuming(take_while1(is_comma)),
    )(i)
}

fn do_parse_gsa(i: &[u8]) -> IResult<&[u8], GsaData> {
    let (i, mode1) = one_of("MA")(i)?;
    let (i, _) = char(',')(i)?;
    let (i, mode2) = one_of("123")(i)?;
    let (i, _) = char(',')(i)?;
    let (i, mut tail) = alt((do_parse_empty_gsa_tail, do_parse_gsa_tail))(i)?;
    Ok((
        i,
        GsaData {
            mode1: match mode1 {
                'M' => GsaMode1::Manual,
                'A' => GsaMode1::Automatic,
                _ => unreachable!(),
            },
            mode2: match mode2 {
                '1' => GsaMode2::NoFix,
                '2' => GsaMode2::Fix2D,
                '3' => GsaMode2::Fix3D,
                _ => unreachable!(),
            },
            fix_sats_prn: tail.0.drain(..).filter_map(|v| v).collect(),
            pdop: tail.1,
            hdop: tail.2,
            vdop: tail.3,
        },
    ))
}

/// Parse GSA
/// from gpsd:
/// eg1. $GPGSA,A,3,,,,,,16,18,,22,24,,,3.6,2.1,2.2*3C
/// eg2. $GPGSA,A,3,19,28,14,18,27,22,31,39,,,,,1.7,1.0,1.3*35
/// 1    = Mode:
/// M=Manual, forced to operate in 2D or 3D
/// A=Automatic, 3D/2D
/// 2    = Mode: 1=Fix not available, 2=2D, 3=3D
/// 3-14 = PRNs of satellites used in position fix (null for unused fields)
/// 15   = PDOP
/// 16   = HDOP
/// 17   = VDOP
///
/// Not all documentation specifies the number of PRN fields, it
/// may be variable.  Most doc that specifies says 12 PRNs.
///
/// the CH-4701 ourputs 24 PRNs!
///
/// The Skytraq S2525F8-BD-RTK output both GPGSA and BDGSA in the
/// same cycle:
/// $GPGSA,A,3,23,31,22,16,03,07,,,,,,,1.8,1.1,1.4*3E
/// $BDGSA,A,3,214,,,,,,,,,,,,1.8,1.1,1.4*18
/// These need to be combined like GPGSV and BDGSV
///
/// Some GPS emit GNGSA.  So far we have not seen a GPS emit GNGSA
/// and then another flavor of xxGSA
///
/// Some Skytraq will emit all GPS in one GNGSA, Then follow with
/// another GNGSA with the BeiDou birds.
///
/// SEANEXX and others also do it:
/// $GNGSA,A,3,31,26,21,,,,,,,,,,3.77,2.55,2.77*1A
/// $GNGSA,A,3,75,86,87,,,,,,,,,,3.77,2.55,2.77*1C
/// seems like the first is GNSS and the second GLONASS
///
/// One chipset called the i.Trek M3 issues GPGSA lines that look like
/// this: "$GPGSA,A,1,,,,*32" when it has no fix.  This is broken
/// in at least two ways: it's got the wrong number of fields, and
/// it claims to be a valid sentence (A flag) when it isn't.
/// Alarmingly, it's possible this error may be generic to SiRFstarIII
fn parse_gsa(s: &NmeaSentence) -> Result<GsaData, String> {
    if s.message_id != b"GSA" {
        return Err("GSA message should starts with $..GSA".into());
    }
    let ret: GsaData = do_parse_gsa(s.data)
        .map(|(_, data)| data)
        .map_err(|err| match err {
            nom::Err::Incomplete(_) => "Incomplete nmea sentence".to_string(),
            nom::Err::Error((_, kind)) | nom::Err::Failure((_, kind)) => {
                kind.description().to_string()
            }
        })?;
    Ok(ret)
}

#[derive(Debug, PartialEq)]
pub struct VtgData {
    pub true_course: Option<f32>,
    pub speed_over_ground: Option<f32>,
}

fn do_parse_vtg(i: &[u8]) -> IResult<&[u8], VtgData> {
    let (i, true_course) = opt(float)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _) = opt(char('T'))(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _magn_course) = opt(float)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _) = opt(char('M'))(i)?;
    let (i, _) = char(',')(i)?;
    let (i, knots_ground_speed) = opt(float)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _) = opt(char('N'))(i)?;
    let (i, kph_ground_speed) = opt(float)(i)?;
    let (i, _) = char(',')(i)?;
    let (i, _) = opt(char('K'))(i)?;

    Ok((
        i,
        VtgData {
            true_course,
            speed_over_ground: match (knots_ground_speed, kph_ground_speed) {
                (Some(val), _) => Some(val),
                (_, Some(val)) => Some(val / 1.852),
                (None, None) => None,
            },
        },
    ))
}

/// parse VTG
/// from http://aprs.gids.nl/nmea/#vtg
/// Track Made Good and Ground Speed.
///
/// eg1. $GPVTG,360.0,T,348.7,M,000.0,N,000.0,K*43
/// eg2. $GPVTG,054.7,T,034.4,M,005.5,N,010.2,K
///
///
/// 054.7,T      True track made good
/// 034.4,M      Magnetic track made good
/// 005.5,N      Ground speed, knots
/// 010.2,K      Ground speed, Kilometers per hour
///
///
/// eg3. $GPVTG,t,T,,,s.ss,N,s.ss,K*hh
/// 1    = Track made good
/// 2    = Fixed text 'T' indicates that track made good is relative to true north
/// 3    = not used
/// 4    = not used
/// 5    = Speed over ground in knots
/// 6    = Fixed text 'N' indicates that speed over ground in in knots
/// 7    = Speed over ground in kilometers/hour
/// 8    = Fixed text 'K' indicates that speed over ground is in kilometers/hour
/// 9    = Checksum
/// The actual track made good and speed relative to the ground.
///
/// $--VTG,x.x,T,x.x,M,x.x,N,x.x,K
/// x.x,T = Track, degrees True
/// x.x,M = Track, degrees Magnetic
/// x.x,N = Speed, knots
/// x.x,K = Speed, Km/hr
fn parse_vtg(s: &NmeaSentence) -> Result<VtgData, String> {
    if s.message_id != b"VTG" {
        return Err("VTG message should starts with $..VTG".into());
    }
    let ret: VtgData = do_parse_vtg(s.data)
        .map(|(_, data)| data)
        .map_err(|err| match err {
            nom::Err::Incomplete(_) => "Incomplete nmea sentence".to_string(),
            nom::Err::Error((_, kind)) | nom::Err::Failure((_, kind)) => {
                kind.description().to_string()
            }
        })?;
    Ok(ret)
}

/// Parse GPGLL (Geographic position)
/// From https://docs.novatel.com/OEM7/Content/Logs/GPGLL.htm
///
/// | Field | Structure   | Description
/// |-------|-------------|---------------------------------------------------------------------
/// | 1     | $GPGLL      | Log header.
/// | 2     | lat         | Latitude (DDmm.mm)
/// | 3     | lat dir     | Latitude direction (N = North, S = South)
/// | 4     | lon         | Longitude (DDDmm.mm)
/// | 5     | lon dir     | Longitude direction (E = East, W = West)
/// | 6     | utc         | UTC time status of position (hours/minutes/seconds/decimal seconds)
/// | 7     | data status | Data status: A = Data valid, V = Data invalid
/// | 8     | mode ind    | Positioning system mode indicator, see `PosSystemIndicator`
/// | 9     | *xx         | Check sum
fn parse_gll(s: &NmeaSentence) -> Result<GllData, String> {
    if s.message_id != b"GLL" {
        return Err("GLL message should starts with $..GLL".into());
    }
    let ret = do_parse_gll(s.data)
        .map(|(_, data)| data)
        .map_err(|err| match err {
            nom::Err::Incomplete(_) => "Incomplete nmea sentence".to_string(),
            nom::Err::Error((_, kind)) | nom::Err::Failure((_, kind)) => {
                kind.description().to_string()
            }
        })?;
    Ok(ret)
}

/// Positioning System Mode Indicator (present from NMEA >= 2.3)
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PosSystemIndicator {
    Autonomous,
    Differential,
    EstimatedMode,
    ManualInput,
    DataNotValid,
}

impl From<char> for PosSystemIndicator {
    fn from(b: char) -> Self {
        match b {
            'A' => PosSystemIndicator::Autonomous,
            'D' => PosSystemIndicator::Differential,
            'E' => PosSystemIndicator::EstimatedMode,
            'M' => PosSystemIndicator::ManualInput,
            'N' => PosSystemIndicator::DataNotValid,
            _ => PosSystemIndicator::DataNotValid,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct GllData {
    pub latitude: f64,
    pub longitude: f64,
    pub fix_time: NaiveTime,
    pub mode: Option<PosSystemIndicator>,
}

fn do_parse_gll(i: &[u8]) -> IResult<&[u8], GllData> {
    let (i, (latitude, longitude)) = do_parse_lat_lon(i)?;
    let (i, _) = char(',')(i)?;
    let (i, fix_time) = parse_hms(i)?;
    let (i, _) = take_until(",")(i)?; // decimal ignored
    let (i, _) = char(',')(i)?;
    let (i, _valid) = char('A')(i)?; // A: valid, V: invalid
    let (i, _) = char(',')(i)?;
    let (i, mode) = opt(terminated(
        map(one_of("ADEM"), PosSystemIndicator::from), // ignore 'N' for invalid
        char(','),
    ))(i)?;

    Ok((
        i,
        GllData {
            latitude,
            longitude,
            fix_time,
            mode,
        },
    ))
}

pub enum ParseResult {
    GGA(GgaData),
    RMC(RmcData),
    GSV(GsvData),
    GSA(GsaData),
    VTG(VtgData),
    GLL(GllData),
    Unsupported(SentenceType),
}

/// parse nmea 0183 sentence and extract data from it
pub fn parse(xs: &[u8]) -> Result<ParseResult, String> {
    let nmea_sentence = parse_nmea_sentence(xs)?;

    if nmea_sentence.checksum == nmea_sentence.calc_checksum() {
        match SentenceType::try_from(nmea_sentence.message_id)? {
            SentenceType::GGA => {
                let data = parse_gga(&nmea_sentence)?;
                Ok(ParseResult::GGA(data))
            }
            SentenceType::GSV => {
                let data = parse_gsv(&nmea_sentence)?;
                Ok(ParseResult::GSV(data))
            }
            SentenceType::RMC => {
                let data = parse_rmc(&nmea_sentence)?;
                Ok(ParseResult::RMC(data))
            }
            SentenceType::GSA => Ok(ParseResult::GSA(parse_gsa(&nmea_sentence)?)),
            SentenceType::VTG => Ok(ParseResult::VTG(parse_vtg(&nmea_sentence)?)),
            SentenceType::GLL => Ok(ParseResult::GLL(parse_gll(&nmea_sentence)?)),
            msg_id => Ok(ParseResult::Unsupported(msg_id)),
        }
    } else {
        Err("Checksum mismatch".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::relative_eq;

    #[test]
    fn test_do_parse_lat_lon() {
        let (_, lat_lon) = do_parse_lat_lon(b"4807.038,N,01131.324,E").unwrap();
        relative_eq!(lat_lon.0, 48. + 7.038 / 60.);
        relative_eq!(lat_lon.1, 11. + 31.324 / 60.);
    }

    #[test]
    fn test_parse_gga_full() {
        let data = parse_gga(&NmeaSentence {
            talker_id: b"GP",
            message_id: b"GGA",
            data: b"033745.0,5650.82344,N,03548.9778,E,1,07,1.8,101.2,M,14.7,M,,",
            checksum: 0x57,
        })
        .unwrap();
        assert_eq!(data.fix_time.unwrap(), NaiveTime::from_hms(3, 37, 45));
        assert_eq!(data.fix_type.unwrap(), FixType::Gps);
        relative_eq!(data.latitude.unwrap(), 56. + 50.82344 / 60.);
        relative_eq!(data.longitude.unwrap(), 35. + 48.9778 / 60.);
        assert_eq!(data.fix_satellites.unwrap(), 7);
        relative_eq!(data.hdop.unwrap(), 1.8);
        relative_eq!(data.altitude.unwrap(), 101.2);
        relative_eq!(data.geoid_height.unwrap(), 14.7);

        let s = parse_nmea_sentence(b"$GPGGA,,,,,,0,,,,,,,,*66").unwrap();
        assert_eq!(s.checksum, s.calc_checksum());
        let data = parse_gga(&s).unwrap();
        assert_eq!(
            GgaData {
                fix_time: None,
                fix_type: Some(FixType::Invalid),
                latitude: None,
                longitude: None,
                fix_satellites: None,
                hdop: None,
                altitude: None,
                geoid_height: None,
            },
            data
        );
    }

    #[test]
    fn test_parse_gga_with_optional_fields() {
        let sentence =
            parse_nmea_sentence(b"$GPGGA,133605.0,5521.75946,N,03731.93769,E,0,00,,,M,,M,,*4F")
                .unwrap();
        assert_eq!(sentence.checksum, sentence.calc_checksum());
        assert_eq!(sentence.checksum, 0x4f);
        let data = parse_gga(&sentence).unwrap();
        assert_eq!(data.fix_type.unwrap(), FixType::Invalid);
    }

    #[test]
    fn test_parse_rmc() {
        let s = parse_nmea_sentence(
            b"$GPRMC,225446.33,A,4916.45,N,12311.12,W,\
                                  000.5,054.7,191194,020.3,E,A*2B",
        )
        .unwrap();
        assert_eq!(s.checksum, s.calc_checksum());
        assert_eq!(s.checksum, 0x2b);
        let rmc_data = parse_rmc(&s).unwrap();
        assert_eq!(
            rmc_data.fix_time.unwrap(),
            NaiveTime::from_hms_milli(22, 54, 46, 330)
        );
        assert_eq!(rmc_data.fix_date.unwrap(), NaiveDate::from_ymd(94, 11, 19));

        println!("lat: {}", rmc_data.lat.unwrap());
        relative_eq!(rmc_data.lat.unwrap(), 49.0 + 16.45 / 60.);
        println!(
            "lon: {}, diff {}",
            rmc_data.lon.unwrap(),
            (rmc_data.lon.unwrap() + (123.0 + 11.12 / 60.)).abs()
        );
        relative_eq!(rmc_data.lon.unwrap(), -(123.0 + 11.12 / 60.));

        relative_eq!(rmc_data.speed_over_ground.unwrap(), 0.5);
        relative_eq!(rmc_data.true_course.unwrap(), 54.7);

        let s = parse_nmea_sentence(b"$GPRMC,,V,,,,,,,,,,N*53").unwrap();
        let rmc = parse_rmc(&s).unwrap();
        assert_eq!(
            RmcData {
                fix_time: None,
                fix_date: None,
                status_of_fix: Some(RmcStatusOfFix::Invalid),
                lat: None,
                lon: None,
                speed_over_ground: None,
                true_course: None,
            },
            rmc
        );
    }

    #[test]
    fn test_parse_gsv_full() {
        let data = parse_gsv(&NmeaSentence {
            talker_id: b"GP",
            message_id: b"GSV",
            data: b"2,1,08,01,,083,46,02,17,308,,12,07,344,39,14,22,228,",
            checksum: 0,
        })
        .unwrap();
        assert_eq!(data.gnss_type, GnssType::Gps);
        assert_eq!(data.number_of_sentences, 2);
        assert_eq!(data.sentence_num, 1);
        assert_eq!(data._sats_in_view, 8);
        assert_eq!(
            data.sats_info[0].clone().unwrap(),
            Satellite {
                gnss_type: data.gnss_type.clone(),
                prn: 1,
                elevation: None,
                azimuth: Some(83.),
                snr: Some(46.),
            }
        );
        assert_eq!(
            data.sats_info[1].clone().unwrap(),
            Satellite {
                gnss_type: data.gnss_type.clone(),
                prn: 2,
                elevation: Some(17.),
                azimuth: Some(308.),
                snr: None,
            }
        );
        assert_eq!(
            data.sats_info[2].clone().unwrap(),
            Satellite {
                gnss_type: data.gnss_type.clone(),
                prn: 12,
                elevation: Some(7.),
                azimuth: Some(344.),
                snr: Some(39.),
            }
        );
        assert_eq!(
            data.sats_info[3].clone().unwrap(),
            Satellite {
                gnss_type: data.gnss_type.clone(),
                prn: 14,
                elevation: Some(22.),
                azimuth: Some(228.),
                snr: None,
            }
        );

        let data = parse_gsv(&NmeaSentence {
            talker_id: b"GL",
            message_id: b"GSV",
            data: b"3,3,10,72,40,075,43,87,00,000,",
            checksum: 0,
        })
        .unwrap();
        assert_eq!(data.gnss_type, GnssType::Glonass);
        assert_eq!(data.number_of_sentences, 3);
        assert_eq!(data.sentence_num, 3);
        assert_eq!(data._sats_in_view, 10);
    }

    #[test]
    fn test_parse_hms() {
        use chrono::Timelike;
        let (_, time) = parse_hms(b"125619,").unwrap();
        assert_eq!(time.hour(), 12);
        assert_eq!(time.minute(), 56);
        assert_eq!(time.second(), 19);
        assert_eq!(time.nanosecond(), 0);
        let (_, time) = parse_hms(b"125619.5,").unwrap();
        assert_eq!(time.hour(), 12);
        assert_eq!(time.minute(), 56);
        assert_eq!(time.second(), 19);
        assert_eq!(time.nanosecond(), 5_00_000_000);
    }

    #[test]
    fn test_gsa_prn_fields_parse() {
        let (_, ret) = gsa_prn_fields_parse(b"5,").unwrap();
        assert_eq!(vec![Some(5)], ret);
        let (_, ret) = gsa_prn_fields_parse(b",").unwrap();
        assert_eq!(vec![None], ret);

        let (_, ret) = gsa_prn_fields_parse(b",,5,6,").unwrap();
        assert_eq!(vec![None, None, Some(5), Some(6)], ret);
    }

    #[test]
    fn smoke_test_parse_gsa() {
        let s = parse_nmea_sentence(b"$GPGSA,A,3,,,,,,16,18,,22,24,,,3.6,2.1,2.2*3C").unwrap();
        let gsa = parse_gsa(&s).unwrap();
        assert_eq!(
            GsaData {
                mode1: GsaMode1::Automatic,
                mode2: GsaMode2::Fix3D,
                fix_sats_prn: vec![16, 18, 22, 24],
                pdop: Some(3.6),
                hdop: Some(2.1),
                vdop: Some(2.2),
            },
            gsa
        );
        let gsa_examples = [
            "$GPGSA,A,3,19,28,14,18,27,22,31,39,,,,,1.7,1.0,1.3*35",
            "$GPGSA,A,3,23,31,22,16,03,07,,,,,,,1.8,1.1,1.4*3E",
            "$BDGSA,A,3,214,,,,,,,,,,,,1.8,1.1,1.4*18",
            "$GNGSA,A,3,31,26,21,,,,,,,,,,3.77,2.55,2.77*1A",
            "$GNGSA,A,3,75,86,87,,,,,,,,,,3.77,2.55,2.77*1C",
            "$GPGSA,A,1,,,,*32",
        ];
        for line in &gsa_examples {
            println!("we parse line '{}'", line);
            let s = parse_nmea_sentence(line.as_bytes()).unwrap();
            parse_gsa(&s).unwrap();
        }
    }

    #[test]
    fn test_parse_vtg() {
        let run_parse_vtg = |line: &str| -> Result<VtgData, String> {
            let s =
                parse_nmea_sentence(line.as_bytes()).expect("VTG sentence initial parse failed");
            assert_eq!(s.checksum, s.calc_checksum());
            parse_vtg(&s)
        };
        assert_eq!(
            VtgData {
                true_course: None,
                speed_over_ground: None,
            },
            run_parse_vtg("$GPVTG,,T,,M,,N,,K,N*2C").unwrap()
        );
        assert_eq!(
            VtgData {
                true_course: Some(360.),
                speed_over_ground: Some(0.),
            },
            run_parse_vtg("$GPVTG,360.0,T,348.7,M,000.0,N,000.0,K*43").unwrap()
        );
        assert_eq!(
            VtgData {
                true_course: Some(54.7),
                speed_over_ground: Some(5.5),
            },
            run_parse_vtg("$GPVTG,054.7,T,034.4,M,005.5,N,010.2,K*48").unwrap()
        );
    }
}
