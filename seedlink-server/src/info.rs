//! XML generation for SeedLink INFO responses (ID, STATIONS, STREAMS, CONNECTIONS).

use crate::connections::ConnectionInfo;
use crate::format_timestamp;
use crate::store::{StationInfo, StreamInfo};

/// Escape XML special characters in attribute values.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Build INFO ID XML response.
pub(crate) fn build_info_id_xml(software: &str, organization: &str, started: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<seedlink software=\"{}\" organization=\"{}\" started=\"{}\"/>\n",
        xml_escape(software),
        xml_escape(organization),
        xml_escape(started),
    )
}

/// Build INFO STATIONS XML response.
pub(crate) fn build_info_stations_xml(stations: &[StationInfo]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\"?>\n<seedlink>\n");
    for s in stations {
        xml.push_str(&format!(
            "  <station name=\"{}\" network=\"{}\" description=\"\" begin_seq=\"{:06X}\" end_seq=\"{:06X}\" stream_check=\"enabled\"/>\n",
            xml_escape(&s.station),
            xml_escape(&s.network),
            s.begin_seq,
            s.end_seq,
        ));
    }
    xml.push_str("</seedlink>\n");
    xml
}

/// Build INFO STREAMS XML response.
pub(crate) fn build_info_streams_xml(streams: &[StreamInfo]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\"?>\n<seedlink>\n");

    // Group streams by (network, station)
    let mut current_station: Option<(&str, &str)> = None;
    for s in streams {
        let is_same = current_station
            .map(|(net, sta)| net == s.network && sta == s.station)
            .unwrap_or(false);

        if !is_same {
            if current_station.is_some() {
                xml.push_str("  </station>\n");
            }
            xml.push_str(&format!(
                "  <station name=\"{}\" network=\"{}\">\n",
                xml_escape(&s.station),
                xml_escape(&s.network),
            ));
            current_station = Some((&s.network, &s.station));
        }

        xml.push_str(&format!(
            "    <stream seedname=\"{}\" location=\"{}\" type=\"{}\" begin_seq=\"{:06X}\" end_seq=\"{:06X}\"/>\n",
            xml_escape(&s.channel),
            xml_escape(&s.location),
            xml_escape(&s.type_code),
            s.begin_seq,
            s.end_seq,
        ));
    }

    if current_station.is_some() {
        xml.push_str("  </station>\n");
    }
    xml.push_str("</seedlink>\n");
    xml
}

/// Build INFO CONNECTIONS XML response.
pub(crate) fn build_info_connections_xml(connections: &[ConnectionInfo]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\"?>\n<seedlink>\n");
    for c in connections {
        let ctime = format_timestamp(c.connected_at);
        let host = xml_escape(&c.addr.to_string());
        let port = c.addr.port();
        let ua = c.user_agent.as_deref().map(xml_escape).unwrap_or_default();
        let proto = match c.protocol_version {
            seedlink_rs_protocol::ProtocolVersion::V3 => "3.1",
            seedlink_rs_protocol::ProtocolVersion::V4 => "4.0",
        };
        xml.push_str(&format!(
            "  <connection host=\"{host}\" port=\"{port}\" ctime=\"{ctime}\" proto=\"{proto}\" useragent=\"{ua}\" state=\"{}\"/>\n",
            xml_escape(&c.state),
        ));
    }
    xml.push_str("</seedlink>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_special_chars() {
        assert_eq!(xml_escape("a&b<c>d\"e"), "a&amp;b&lt;c&gt;d&quot;e");
    }

    #[test]
    fn xml_escape_no_special() {
        assert_eq!(xml_escape("hello"), "hello");
    }

    #[test]
    fn info_id_xml() {
        let xml = build_info_id_xml("SeedLink v3.1", "seedlink-rs", "2026/02/12 10:30:00");
        assert!(xml.contains("software=\"SeedLink v3.1\""));
        assert!(xml.contains("organization=\"seedlink-rs\""));
        assert!(xml.contains("started=\"2026/02/12 10:30:00\""));
    }

    #[test]
    fn info_stations_xml() {
        let stations = vec![
            StationInfo {
                network: "IU".into(),
                station: "ANMO".into(),
                begin_seq: 1,
                end_seq: 5,
            },
            StationInfo {
                network: "GE".into(),
                station: "WLF".into(),
                begin_seq: 2,
                end_seq: 3,
            },
        ];
        let xml = build_info_stations_xml(&stations);
        assert!(xml.contains("name=\"ANMO\""));
        assert!(xml.contains("network=\"IU\""));
        assert!(xml.contains("begin_seq=\"000001\""));
        assert!(xml.contains("end_seq=\"000005\""));
        assert!(xml.contains("name=\"WLF\""));
        assert!(xml.contains("network=\"GE\""));
    }

    #[test]
    fn info_streams_xml() {
        let streams = vec![
            StreamInfo {
                network: "IU".into(),
                station: "ANMO".into(),
                channel: "BHZ".into(),
                location: "00".into(),
                type_code: "D".into(),
                begin_seq: 1,
                end_seq: 3,
            },
            StreamInfo {
                network: "IU".into(),
                station: "ANMO".into(),
                channel: "BHN".into(),
                location: "00".into(),
                type_code: "D".into(),
                begin_seq: 2,
                end_seq: 4,
            },
        ];
        let xml = build_info_streams_xml(&streams);
        assert!(xml.contains("<station name=\"ANMO\" network=\"IU\">"));
        assert!(xml.contains("seedname=\"BHZ\""));
        assert!(xml.contains("seedname=\"BHN\""));
        // Should only have one station open/close
        assert_eq!(xml.matches("<station ").count(), 1);
        assert_eq!(xml.matches("</station>").count(), 1);
    }

    #[test]
    fn info_streams_xml_multiple_stations() {
        let streams = vec![
            StreamInfo {
                network: "GE".into(),
                station: "WLF".into(),
                channel: "BHZ".into(),
                location: "00".into(),
                type_code: "D".into(),
                begin_seq: 1,
                end_seq: 1,
            },
            StreamInfo {
                network: "IU".into(),
                station: "ANMO".into(),
                channel: "BHZ".into(),
                location: "00".into(),
                type_code: "D".into(),
                begin_seq: 2,
                end_seq: 2,
            },
        ];
        let xml = build_info_streams_xml(&streams);
        assert_eq!(xml.matches("<station ").count(), 2);
        assert_eq!(xml.matches("</station>").count(), 2);
    }
}
