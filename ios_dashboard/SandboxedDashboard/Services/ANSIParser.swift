//
//  ANSIParser.swift
//  SandboxedDashboard
//
//  State machine-based ANSI/VT100 escape sequence parser
//  Based on: https://vt100.net/emu/dec_ansi_parser
//           https://github.com/haberman/vtparse
//

import SwiftUI

/// A proper state machine parser for ANSI/VT100 escape sequences
/// Converts terminal output to AttributedString with colors
final class ANSIParser {
    
    // MARK: - Types
    
    enum State {
        case ground
        case escape
        case escapeIntermediate
        case csiEntry
        case csiParam
        case csiIntermediate
        case csiIgnore
        case oscString
        case dcsEntry
        case dcsParam
        case dcsIntermediate
        case dcsPassthrough
        case dcsIgnore
        case sosPmApcString
    }
    
    struct TextStyle {
        var foreground: Color = .white
        var background: Color? = nil
        var bold: Bool = false
        var dim: Bool = false
        var italic: Bool = false
        var underline: Bool = false
        var blink: Bool = false
        var inverse: Bool = false
        var hidden: Bool = false
        var strikethrough: Bool = false
        
        mutating func reset() {
            foreground = .white
            background = nil
            bold = false
            dim = false
            italic = false
            underline = false
            blink = false
            inverse = false
            hidden = false
            strikethrough = false
        }
        
        var effectiveForeground: Color {
            let base = inverse ? (background ?? Color(white: 0.1)) : foreground
            return dim ? base.opacity(0.6) : base
        }
        
        var effectiveBackground: Color? {
            inverse ? foreground : background
        }
    }
    
    // MARK: - State
    
    private var state: State = .ground
    private var intermediates: [UInt8] = []
    private var params: [Int] = []
    private var currentParam: Int = 0
    private var hasCurrentParam: Bool = false
    
    private var style = TextStyle()
    private var result = AttributedString()
    private var currentText = ""
    
    // MARK: - Public API
    
    /// Parse ANSI text and return AttributedString with colors
    static func parse(_ text: String) -> AttributedString {
        let parser = ANSIParser()
        return parser.process(text)
    }
    
    /// Process input text and return attributed string
    func process(_ text: String) -> AttributedString {
        result = AttributedString()
        currentText = ""
        
        for scalar in text.unicodeScalars {
            let byte = UInt8(min(scalar.value, 255))
            processByte(byte, char: Character(scalar))
        }
        
        // Flush any remaining text
        flushText()
        
        return result
    }
    
    // MARK: - State Machine
    
    private func processByte(_ byte: UInt8, char: Character) {
        // Handle "anywhere" transitions first
        switch byte {
        case 0x18, 0x1A: // CAN, SUB - cancel sequence
            flushText()
            clear()
            state = .ground
            return
        case 0x1B: // ESC - start escape sequence
            flushText()
            clear()
            state = .escape
            return
        case 0x9B: // CSI (8-bit)
            flushText()
            clear()
            state = .csiEntry
            return
        case 0x9D: // OSC (8-bit)
            flushText()
            clear()
            state = .oscString
            return
        case 0x90: // DCS (8-bit)
            flushText()
            clear()
            state = .dcsEntry
            return
        case 0x98, 0x9E, 0x9F: // SOS, PM, APC (8-bit)
            flushText()
            clear()
            state = .sosPmApcString
            return
        case 0x9C: // ST (String Terminator)
            flushText()
            state = .ground
            return
        default:
            break
        }
        
        // State-specific handling
        switch state {
        case .ground:
            handleGround(byte, char: char)
        case .escape:
            handleEscape(byte)
        case .escapeIntermediate:
            handleEscapeIntermediate(byte)
        case .csiEntry:
            handleCsiEntry(byte)
        case .csiParam:
            handleCsiParam(byte)
        case .csiIntermediate:
            handleCsiIntermediate(byte)
        case .csiIgnore:
            handleCsiIgnore(byte)
        case .oscString, .sosPmApcString:
            handleStringState(byte)
        case .dcsEntry:
            handleDcsEntry(byte)
        case .dcsParam:
            handleDcsParam(byte)
        case .dcsIntermediate:
            handleDcsIntermediate(byte)
        case .dcsPassthrough:
            handleDcsPassthrough(byte)
        case .dcsIgnore:
            handleDcsIgnore(byte)
        }
    }
    
    // MARK: - State Handlers
    
    private func handleGround(_ byte: UInt8, char: Character) {
        switch byte {
        case 0x00...0x1F:
            // C0 controls - execute (mostly ignore for display)
            if byte == 0x0A { // LF
                currentText.append("\n")
            } else if byte == 0x0D { // CR
                // Ignore CR (usually paired with LF)
            } else if byte == 0x09 { // TAB
                currentText.append("\t")
            }
            // Other C0 controls ignored
        case 0x20...0x7E:
            // Printable ASCII
            currentText.append(char)
        case 0x7F:
            // DEL - ignore
            break
        case 0xA0...0xFE:
            // Printable high bytes (treat like GL)
            currentText.append(char)
        default:
            break
        }
    }
    
    private func handleEscape(_ byte: UInt8) {
        switch byte {
        case 0x00...0x1F:
            // C0 controls - execute
            break
        case 0x20...0x2F:
            // Intermediate - collect and transition
            intermediates.append(byte)
            state = .escapeIntermediate
        case 0x30...0x4F, 0x51...0x57, 0x59, 0x5A, 0x5C, 0x60...0x7E:
            // Final characters - dispatch escape sequence
            dispatchEscape(byte)
            state = .ground
        case 0x5B: // '[' - CSI
            clear()
            state = .csiEntry
        case 0x5D: // ']' - OSC
            clear()
            state = .oscString
        case 0x50: // 'P' - DCS
            clear()
            state = .dcsEntry
        case 0x58, 0x5E, 0x5F: // 'X', '^', '_' - SOS, PM, APC
            clear()
            state = .sosPmApcString
        case 0x7F:
            // DEL - ignore
            break
        default:
            state = .ground
        }
    }
    
    private func handleEscapeIntermediate(_ byte: UInt8) {
        switch byte {
        case 0x00...0x1F:
            // C0 controls - execute
            break
        case 0x20...0x2F:
            // More intermediates
            intermediates.append(byte)
        case 0x30...0x7E:
            // Final - dispatch
            dispatchEscape(byte)
            state = .ground
        case 0x7F:
            // DEL - ignore
            break
        default:
            state = .ground
        }
    }
    
    private func handleCsiEntry(_ byte: UInt8) {
        switch byte {
        case 0x00...0x1F:
            // C0 controls - execute
            break
        case 0x20...0x2F:
            // Intermediate
            intermediates.append(byte)
            state = .csiIntermediate
        case 0x30...0x39: // '0'-'9'
            currentParam = Int(byte - 0x30)
            hasCurrentParam = true
            state = .csiParam
        case 0x3A: // ':' - subparameter (ignore sequence)
            state = .csiIgnore
        case 0x3B: // ';' - parameter separator
            params.append(0) // Default value
            state = .csiParam
        case 0x3C...0x3F: // '<', '=', '>', '?' - private marker
            intermediates.append(byte)
            state = .csiParam
        case 0x40...0x7E:
            // Final - dispatch
            dispatchCsi(byte)
            state = .ground
        case 0x7F:
            // DEL - ignore
            break
        default:
            state = .ground
        }
    }
    
    private func handleCsiParam(_ byte: UInt8) {
        switch byte {
        case 0x00...0x1F:
            // C0 controls - execute
            break
        case 0x20...0x2F:
            // Intermediate
            if hasCurrentParam {
                params.append(currentParam)
                currentParam = 0
                hasCurrentParam = false
            }
            intermediates.append(byte)
            state = .csiIntermediate
        case 0x30...0x39: // '0'-'9'
            currentParam = currentParam * 10 + Int(byte - 0x30)
            hasCurrentParam = true
        case 0x3A: // ':' - subparameter
            state = .csiIgnore
        case 0x3B: // ';' - parameter separator
            params.append(hasCurrentParam ? currentParam : 0)
            currentParam = 0
            hasCurrentParam = false
        case 0x3C...0x3F: // Private markers in wrong position
            state = .csiIgnore
        case 0x40...0x7E:
            // Final - dispatch
            if hasCurrentParam {
                params.append(currentParam)
            }
            dispatchCsi(byte)
            state = .ground
        case 0x7F:
            // DEL - ignore
            break
        default:
            state = .ground
        }
    }
    
    private func handleCsiIntermediate(_ byte: UInt8) {
        switch byte {
        case 0x00...0x1F:
            // C0 controls - execute
            break
        case 0x20...0x2F:
            // More intermediates
            intermediates.append(byte)
        case 0x30...0x3F:
            // Parameters after intermediate - error
            state = .csiIgnore
        case 0x40...0x7E:
            // Final - dispatch
            dispatchCsi(byte)
            state = .ground
        case 0x7F:
            // DEL - ignore
            break
        default:
            state = .ground
        }
    }
    
    private func handleCsiIgnore(_ byte: UInt8) {
        switch byte {
        case 0x00...0x1F:
            // C0 controls - execute
            break
        case 0x20...0x3F:
            // Ignore
            break
        case 0x40...0x7E:
            // Final - transition to ground (no dispatch)
            state = .ground
        case 0x7F:
            // DEL - ignore
            break
        default:
            state = .ground
        }
    }
    
    private func handleStringState(_ byte: UInt8) {
        // OSC, SOS, PM, APC strings - ignore until ST
        switch byte {
        case 0x07: // BEL - alternative terminator for OSC
            state = .ground
        case 0x00...0x1F:
            // Ignore most C0
            break
        default:
            // Ignore string content
            break
        }
    }
    
    private func handleDcsEntry(_ byte: UInt8) {
        // Similar to CSI entry but for device control strings
        switch byte {
        case 0x20...0x2F:
            intermediates.append(byte)
            state = .dcsIntermediate
        case 0x30...0x39, 0x3B:
            state = .dcsParam
        case 0x3C...0x3F:
            intermediates.append(byte)
            state = .dcsParam
        case 0x40...0x7E:
            state = .dcsPassthrough
        default:
            break
        }
    }
    
    private func handleDcsParam(_ byte: UInt8) {
        switch byte {
        case 0x30...0x39, 0x3B:
            break // Collect params
        case 0x20...0x2F:
            state = .dcsIntermediate
        case 0x40...0x7E:
            state = .dcsPassthrough
        case 0x3A, 0x3C...0x3F:
            state = .dcsIgnore
        default:
            break
        }
    }
    
    private func handleDcsIntermediate(_ byte: UInt8) {
        switch byte {
        case 0x20...0x2F:
            break // More intermediates
        case 0x40...0x7E:
            state = .dcsPassthrough
        case 0x30...0x3F:
            state = .dcsIgnore
        default:
            break
        }
    }
    
    private func handleDcsPassthrough(_ byte: UInt8) {
        // Ignore DCS content
    }
    
    private func handleDcsIgnore(_ byte: UInt8) {
        // Ignore until ST
    }
    
    // MARK: - Dispatch
    
    private func dispatchEscape(_ final: UInt8) {
        // Most escape sequences we don't care about for display
        // Could handle things like ESC 7 (save cursor) if needed
    }
    
    private func dispatchCsi(_ final: UInt8) {
        // Check for private marker
        let isPrivate = !intermediates.isEmpty && intermediates[0] >= 0x3C && intermediates[0] <= 0x3F
        
        switch final {
        case 0x6D: // 'm' - SGR (Select Graphic Rendition)
            if !isPrivate {
                handleSGR()
            }
        default:
            // Other CSI sequences (cursor movement, etc.) - ignore for display
            break
        }
    }
    
    // MARK: - SGR (Colors and Styles)
    
    private func handleSGR() {
        // If no parameters, treat as reset
        if params.isEmpty {
            style.reset()
            return
        }
        
        var i = 0
        while i < params.count {
            let code = params[i]
            
            switch code {
            case 0:
                style.reset()
            case 1:
                style.bold = true
            case 2:
                style.dim = true
            case 3:
                style.italic = true
            case 4:
                style.underline = true
            case 5, 6:
                style.blink = true
            case 7:
                style.inverse = true
            case 8:
                style.hidden = true
            case 9:
                style.strikethrough = true
            case 22:
                style.bold = false
                style.dim = false
            case 23:
                style.italic = false
            case 24:
                style.underline = false
            case 25:
                style.blink = false
            case 27:
                style.inverse = false
            case 28:
                style.hidden = false
            case 29:
                style.strikethrough = false
                
            // Foreground colors (30-37)
            case 30: style.foreground = Color(white: 0.2)
            case 31: style.foreground = Color(red: 0.94, green: 0.33, blue: 0.31)
            case 32: style.foreground = Color(red: 0.33, green: 0.86, blue: 0.43)
            case 33: style.foreground = Color(red: 0.98, green: 0.74, blue: 0.25)
            case 34: style.foreground = Color(red: 0.40, green: 0.57, blue: 0.93)
            case 35: style.foreground = Color(red: 0.83, green: 0.42, blue: 0.78)
            case 36: style.foreground = Color(red: 0.30, green: 0.82, blue: 0.87)
            case 37: style.foreground = Color(white: 0.9)
            case 39: style.foreground = .white
                
            // Background colors (40-47)
            case 40: style.background = Color(white: 0.1)
            case 41: style.background = Color(red: 0.6, green: 0.15, blue: 0.15)
            case 42: style.background = Color(red: 0.15, green: 0.5, blue: 0.2)
            case 43: style.background = Color(red: 0.6, green: 0.45, blue: 0.1)
            case 44: style.background = Color(red: 0.15, green: 0.25, blue: 0.55)
            case 45: style.background = Color(red: 0.5, green: 0.2, blue: 0.45)
            case 46: style.background = Color(red: 0.1, green: 0.45, blue: 0.5)
            case 47: style.background = Color(white: 0.7)
            case 49: style.background = nil
                
            // Bright foreground (90-97)
            case 90: style.foreground = Color(white: 0.5)
            case 91: style.foreground = Color(red: 1, green: 0.45, blue: 0.45)
            case 92: style.foreground = Color(red: 0.45, green: 1, blue: 0.55)
            case 93: style.foreground = Color(red: 1, green: 0.9, blue: 0.45)
            case 94: style.foreground = Color(red: 0.55, green: 0.7, blue: 1)
            case 95: style.foreground = Color(red: 1, green: 0.55, blue: 0.95)
            case 96: style.foreground = Color(red: 0.45, green: 0.95, blue: 1)
            case 97: style.foreground = .white
                
            // Bright background (100-107)
            case 100: style.background = Color(white: 0.4)
            case 101: style.background = Color(red: 0.8, green: 0.3, blue: 0.3)
            case 102: style.background = Color(red: 0.3, green: 0.7, blue: 0.35)
            case 103: style.background = Color(red: 0.8, green: 0.65, blue: 0.2)
            case 104: style.background = Color(red: 0.35, green: 0.45, blue: 0.75)
            case 105: style.background = Color(red: 0.7, green: 0.4, blue: 0.65)
            case 106: style.background = Color(red: 0.25, green: 0.65, blue: 0.7)
            case 107: style.background = Color(white: 0.85)
                
            // 256 color mode (38;5;n or 48;5;n)
            case 38:
                if i + 2 < params.count && params[i + 1] == 5 {
                    style.foreground = color256(params[i + 2])
                    i += 2
                } else if i + 4 < params.count && params[i + 1] == 2 {
                    // True color: 38;2;r;g;b
                    style.foreground = Color(
                        red: Double(params[i + 2]) / 255.0,
                        green: Double(params[i + 3]) / 255.0,
                        blue: Double(params[i + 4]) / 255.0
                    )
                    i += 4
                }
            case 48:
                if i + 2 < params.count && params[i + 1] == 5 {
                    style.background = color256(params[i + 2])
                    i += 2
                } else if i + 4 < params.count && params[i + 1] == 2 {
                    // True color: 48;2;r;g;b
                    style.background = Color(
                        red: Double(params[i + 2]) / 255.0,
                        green: Double(params[i + 3]) / 255.0,
                        blue: Double(params[i + 4]) / 255.0
                    )
                    i += 4
                }
                
            default:
                break
            }
            
            i += 1
        }
    }
    
    // MARK: - Helpers
    
    private func clear() {
        intermediates.removeAll()
        params.removeAll()
        currentParam = 0
        hasCurrentParam = false
    }
    
    private func flushText() {
        guard !currentText.isEmpty else { return }
        
        var attr = AttributedString(currentText)
        attr.foregroundColor = style.effectiveForeground
        attr.font = .system(size: 13, weight: style.bold ? .bold : .regular, design: .monospaced)
        
        if let bg = style.effectiveBackground {
            attr.backgroundColor = bg
        }
        if style.underline {
            attr.underlineStyle = .single
        }
        if style.strikethrough {
            attr.strikethroughStyle = .single
        }
        
        result.append(attr)
        currentText = ""
    }
    
    private func color256(_ index: Int) -> Color {
        guard index >= 0 && index < 256 else { return .white }
        
        if index < 16 {
            // Standard colors
            let colors: [Color] = [
                Color(white: 0.1),                              // 0: Black
                Color(red: 0.8, green: 0.2, blue: 0.2),         // 1: Red
                Color(red: 0.2, green: 0.8, blue: 0.3),         // 2: Green
                Color(red: 0.8, green: 0.7, blue: 0.2),         // 3: Yellow
                Color(red: 0.3, green: 0.4, blue: 0.9),         // 4: Blue
                Color(red: 0.8, green: 0.3, blue: 0.7),         // 5: Magenta
                Color(red: 0.2, green: 0.7, blue: 0.8),         // 6: Cyan
                Color(white: 0.85),                             // 7: White
                Color(white: 0.4),                              // 8: Bright Black
                Color(red: 1, green: 0.4, blue: 0.4),           // 9: Bright Red
                Color(red: 0.4, green: 1, blue: 0.5),           // 10: Bright Green
                Color(red: 1, green: 0.95, blue: 0.4),          // 11: Bright Yellow
                Color(red: 0.5, green: 0.6, blue: 1),           // 12: Bright Blue
                Color(red: 1, green: 0.5, blue: 0.9),           // 13: Bright Magenta
                Color(red: 0.4, green: 0.95, blue: 1),          // 14: Bright Cyan
                .white                                          // 15: Bright White
            ]
            return colors[index]
        } else if index < 232 {
            // 216 color cube (6x6x6)
            let n = index - 16
            let b = n % 6
            let g = (n / 6) % 6
            let r = n / 36
            return Color(
                red: r == 0 ? 0 : Double(r * 40 + 55) / 255.0,
                green: g == 0 ? 0 : Double(g * 40 + 55) / 255.0,
                blue: b == 0 ? 0 : Double(b * 40 + 55) / 255.0
            )
        } else {
            // Grayscale (24 shades)
            let gray = Double((index - 232) * 10 + 8) / 255.0
            return Color(white: gray)
        }
    }
}
