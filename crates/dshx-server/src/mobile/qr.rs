//! QR codes of a pairing link: an SVG for the web UI and Unicode half-blocks
//! for the terminal.
use qrcode::types::Color;
use qrcode::{EcLevel, QrCode};

/// Quiet zone of the SVG, in modules (the standard's four).
pub const SVG_QUIET: usize = 4;
/// Quiet zone in the terminal, in modules: two keep the code compact and
/// still scan from a screen.
pub const TERMINAL_QUIET: usize = 2;

/// The modules of a code, row by row; `true` is dark.
pub struct Matrix {
    pub width: usize,
    pub dark: Vec<bool>,
}

impl Matrix {
    /// Dark or not; anything outside the code is light.
    pub fn get(&self, x: isize, y: isize) -> bool {
        if x < 0 || y < 0 || x >= self.width as isize || y >= self.width as isize {
            return false;
        }
        self.dark[y as usize * self.width + x as usize]
    }

    pub fn dark_count(&self) -> usize {
        self.dark.iter().filter(|d| **d).count()
    }
}

/// Error correction M, or L when the text only fits that way (a payload
/// with an SSH key is long).
pub fn matrix(text: &str) -> anyhow::Result<Matrix> {
    let code = QrCode::with_error_correction_level(text, EcLevel::M)
        .or_else(|_| QrCode::with_error_correction_level(text, EcLevel::L))
        .map_err(|e| anyhow::anyhow!("QR code: {e}"))?;
    let width = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == Color::Dark)
        .collect();
    Ok(Matrix { width, dark })
}

/// An SVG whose `viewBox` counts modules (quiet zone included), so CSS
/// decides the size; black on white, which scanners expect.
pub fn svg(text: &str) -> anyhow::Result<String> {
    let m = matrix(text)?;
    let size = m.width + 2 * SVG_QUIET;
    let mut d = String::new();
    for y in 0..m.width {
        let mut x = 0;
        while x < m.width {
            if !m.get(x as isize, y as isize) {
                x += 1;
                continue;
            }
            let start = x;
            while x < m.width && m.get(x as isize, y as isize) {
                x += 1;
            }
            let run = x - start;
            d.push_str(&format!(
                "M{} {}h{run}v1h-{run}z",
                start + SVG_QUIET,
                y + SVG_QUIET
            ));
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {size} {size}\" \
         shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"QR code\">\
         <rect width=\"{size}\" height=\"{size}\" fill=\"#fff\"/>\
         <path fill=\"#000\" d=\"{d}\"/></svg>"
    ))
}

/// Two module rows per text line with half blocks and a quiet zone of two.
/// `ansi` paints black on white so the terminal's theme cannot invert it.
pub fn terminal(text: &str, ansi: bool) -> anyhow::Result<String> {
    let m = matrix(text)?;
    let quiet = TERMINAL_QUIET as isize;
    let width = m.width as isize;
    let mut out = String::new();
    let mut y = -quiet;
    while y < width + quiet {
        if ansi {
            out.push_str("\x1b[30;107m");
        }
        for x in -quiet..width + quiet {
            out.push(match (m.get(x, y), m.get(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        if ansi {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
        y += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "aether://pair#eyJ2IjoxfQ";

    #[test]
    fn svg_is_well_formed_and_draws_every_dark_module() {
        let m = matrix(LINK).unwrap();
        assert_eq!(m.width, 21 + 4 * (m.width - 21) / 4, "a QR version's width");
        assert_eq!(m.dark.len(), m.width * m.width);
        let svg = svg(LINK).unwrap();
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.ends_with("</svg>"));
        let size = m.width + 2 * SVG_QUIET;
        assert!(svg.contains(&format!("viewBox=\"0 0 {size} {size}\"")));
        assert_eq!(svg.matches('<').count(), svg.matches('>').count());
        // The path's horizontal runs cover exactly the dark modules.
        let d = svg.split("d=\"").nth(1).unwrap().split('"').next().unwrap();
        let drawn: usize = d
            .split('M')
            .filter(|s| !s.is_empty())
            .map(|run| {
                let h = run.split('h').nth(1).unwrap();
                h.split('v').next().unwrap().parse::<usize>().unwrap()
            })
            .sum();
        assert_eq!(drawn, m.dark_count());
        // Finder pattern: the top-left module is dark and sits inside the quiet zone.
        assert!(d.starts_with(&format!("M{} {}h7", SVG_QUIET, SVG_QUIET)));
    }

    #[test]
    fn terminal_uses_half_blocks_with_a_quiet_zone_of_two() {
        let m = matrix(LINK).unwrap();
        let plain = terminal(LINK, false).unwrap();
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines.len(), (m.width + 2 * TERMINAL_QUIET).div_ceil(2));
        for line in &lines {
            assert_eq!(line.chars().count(), m.width + 2 * TERMINAL_QUIET);
            assert!(line.chars().all(|c| matches!(c, '█' | '▀' | '▄' | ' ')));
        }
        assert!(lines[0].starts_with("  "), "quiet zone on the left");
        assert!(lines[0].ends_with("  "), "quiet zone on the right");
        let ansi = terminal(LINK, true).unwrap();
        assert!(ansi.contains("\x1b[30;107m") && ansi.ends_with("\x1b[0m\n"));
    }

    #[test]
    fn long_payloads_fall_back_to_level_l() {
        let long = "x".repeat(2500);
        assert!(matrix(&long).is_ok());
        assert!(matrix(&"x".repeat(3000)).is_err());
    }
}
