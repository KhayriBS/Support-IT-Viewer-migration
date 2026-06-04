// Détection de l'IP physique active + filtre des candidats ICE indésirables.
//
// Sur des postes Windows avec NordVPN (WireGuard 10.5.x.x), VirtualBox
// (192.168.56.x) ou VMware (192.168.30.x / 192.168.9.x), webrtc-rs énumère
// toutes les interfaces et propose souvent un candidat host sur une carte
// virtuelle ou VPN. Le viewer en sélectionne une qu'il ne peut pas joindre
// → "Peer not connected: agent". On bloque ces préfixes avant l'envoi au
// signaling pour forcer ICE à converger sur le LAN physique.

use std::net::UdpSocket;

/// Préfixes d'adresses qu'on refuse d'annoncer comme candidats ICE.
/// Étendre cette liste si une nouvelle interface virtuelle apparaît.
const BLOCKED_IP_PREFIXES: &[&str] = &[
    // VPN WireGuard / NordLynx
    "10.5.",
    "10.6.",
    // VirtualBox Host-Only
    "192.168.56.",
    // VMware VMnet1 / VMnet8
    "192.168.30.",
    "192.168.9.",
    // APIPA (lien-local auto-config IPv4)
    "169.254.",
    // IPv6 link-local
    "fe80:",
    "fe80::",
];

/// Découvre l'IP locale physique réellement active vers Internet.
///
/// Le truc UDP : on `bind` sur `0.0.0.0:0` (toutes interfaces) puis on
/// "connecte" la socket vers 8.8.8.8:80. Aucun paquet n'est envoyé
/// (UDP est sans connexion), mais l'OS doit choisir l'interface de sortie
/// → on récupère son IP via `local_addr()`. Avantage : ça correspond à la
/// route par défaut, donc à l'interface que les autres peers atteindront.
pub fn get_local_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}

/// `false` si le candidat ICE pointe sur une interface VPN/virtuelle/APIPA.
///
/// On parse l'IP depuis la ligne `candidate:…` (format SDP) et on teste
/// chaque préfixe de [`BLOCKED_IP_PREFIXES`]. Insensible à la casse pour
/// l'IPv6 link-local.
pub fn is_valid_ice_candidate(candidate_line: &str) -> bool {
    let Some(ip) = extract_ip_from_candidate(candidate_line) else {
        // Pas d'IP parsable (mDNS `.local` par exemple) → on laisse passer,
        // le viewer fera lui-même son tri.
        return true;
    };
    let ip_lower = ip.to_ascii_lowercase();
    !BLOCKED_IP_PREFIXES
        .iter()
        .any(|prefix| ip_lower.starts_with(&prefix.to_ascii_lowercase()))
}

/// Extrait le champ `connection-address` d'une ligne candidate SDP.
/// Format: `foundation component protocol priority IP PORT typ <type> …`
fn extract_ip_from_candidate(candidate_line: &str) -> Option<&str> {
    let trimmed = candidate_line.strip_prefix("candidate:").unwrap_or(candidate_line);
    let mut parts = trimmed.split_whitespace();
    parts.next()?; // foundation
    parts.next()?; // component
    parts.next()?; // protocol
    parts.next()?; // priority
    parts.next()   // IP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_vpn_10_5() {
        let cand = "candidate:1 1 udp 2113937151 10.5.0.2 52582 typ host";
        assert!(!is_valid_ice_candidate(cand));
    }

    #[test]
    fn blocks_virtualbox_host_only() {
        let cand = "candidate:1 1 udp 2113937151 192.168.56.1 54620 typ host";
        assert!(!is_valid_ice_candidate(cand));
    }

    #[test]
    fn blocks_vmware_vmnet1() {
        let cand = "candidate:1 1 udp 2113937151 192.168.30.1 54621 typ host";
        assert!(!is_valid_ice_candidate(cand));
    }

    #[test]
    fn blocks_vmware_vmnet8() {
        let cand = "candidate:1 1 udp 2113937151 192.168.9.1 54622 typ host";
        assert!(!is_valid_ice_candidate(cand));
    }

    #[test]
    fn blocks_apipa() {
        let cand = "candidate:1 1 udp 2113937151 169.254.1.1 54620 typ host";
        assert!(!is_valid_ice_candidate(cand));
    }

    #[test]
    fn blocks_ipv6_link_local() {
        let cand = "candidate:1 1 udp 2113937151 fe80::1 54620 typ host";
        assert!(!is_valid_ice_candidate(cand));
    }

    #[test]
    fn accepts_physical_wifi() {
        let cand = "candidate:1 1 udp 2113937151 192.168.1.179 54622 typ host";
        assert!(is_valid_ice_candidate(cand));
    }

    #[test]
    fn accepts_relay() {
        let cand = "candidate:4 1 udp 41886207 172.232.192.83 48802 typ relay raddr 0.0.0.0 rport 0";
        assert!(is_valid_ice_candidate(cand));
    }

    #[test]
    fn accepts_mdns_unparseable() {
        // mDNS local — on n'a pas d'IP, on laisse passer (le viewer décidera)
        let cand = "candidate:1 1 udp 2113937151 abc.local 54620 typ host";
        assert!(is_valid_ice_candidate(cand));
    }
}
