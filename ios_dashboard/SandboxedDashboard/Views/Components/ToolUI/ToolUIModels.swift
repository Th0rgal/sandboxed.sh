//
//  ToolUIModels.swift
//  SandboxedDashboard
//
//  Data models for tool UI components
//

import Foundation

// MARK: - Data Table

struct ToolUIDataTable: Codable, @unchecked Sendable {
    let id: String?
    let title: String?
    let columns: [Column]
    let rows: [[String: AnyCodable]]
    
    struct Column: Codable, Sendable {
        let id: String
        let label: String?
        let width: String?
        
        var displayLabel: String {
            label ?? id.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }
    
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decodeIfPresent(String.self, forKey: .id)
        title = try container.decodeIfPresent(String.self, forKey: .title)
        rows = try container.decodeIfPresent([[String: AnyCodable]].self, forKey: .rows) ?? []
        
        // Columns can be strings or objects
        if let columnObjects = try? container.decode([Column].self, forKey: .columns) {
            columns = columnObjects
        } else if let columnStrings = try? container.decode([String].self, forKey: .columns) {
            columns = columnStrings.map { Column(id: $0, label: $0, width: nil) }
        } else {
            columns = []
        }
    }
    
    enum CodingKeys: String, CodingKey {
        case id, title, columns, rows
    }
}

// MARK: - Option List

struct ToolUIOptionList: Codable, @unchecked Sendable {
    let id: String?
    let options: [Option]
    let selectionMode: String?
    let defaultValue: AnyCodable?
    let confirmed: AnyCodable?
    
    struct Option: Codable, Identifiable, Sendable {
        let id: String
        let label: String
        let description: String?
        let disabled: Bool?
    }
    
    var isSingleSelect: Bool {
        selectionMode != "multi"
    }
    
    var confirmedIds: [String] {
        guard let confirmed = confirmed else { return [] }
        if let str = confirmed.value as? String {
            return [str]
        }
        if let arr = confirmed.value as? [String] {
            return arr
        }
        return []
    }
}

// MARK: - Progress Bar

struct ToolUIProgress: Codable, Sendable {
    let id: String?
    let title: String?
    let current: Int
    let total: Int
    let status: String?

    var percentage: Double {
        guard total > 0 else { return 0 }
        return Double(current) / Double(total)
    }

    var displayText: String {
        "\(current)/\(total)"
    }
}

// MARK: - Alert/Notification

struct ToolUIAlert: Codable, Sendable {
    let id: String?
    let title: String
    let message: String?
    let type: String? // "info", "success", "warning", "error"

    var alertType: AlertType {
        AlertType(rawValue: type ?? "info") ?? .info
    }

    enum AlertType: String, Sendable {
        case info, success, warning, error
    }
}

// MARK: - Code Block

struct ToolUICodeBlock: Codable, Sendable {
    let id: String?
    let title: String?
    let language: String?
    let code: String
    let lineNumbers: Bool?
}

// MARK: - Tool Call Wrapper

enum ToolUIContent: Identifiable, @unchecked Sendable {
    case dataTable(ToolUIDataTable)
    case optionList(ToolUIOptionList)
    case progress(ToolUIProgress)
    case alert(ToolUIAlert)
    case codeBlock(ToolUICodeBlock)
    case unknown(name: String, args: String)

    var id: String {
        switch self {
        case .dataTable(let table):
            return table.id ?? UUID().uuidString
        case .optionList(let list):
            return list.id ?? UUID().uuidString
        case .progress(let progress):
            return progress.id ?? UUID().uuidString
        case .alert(let alert):
            return alert.id ?? UUID().uuidString
        case .codeBlock(let code):
            return code.id ?? UUID().uuidString
        case .unknown(let name, _):
            return "unknown-\(name)"
        }
    }

    static func parse(name: String, args: [String: Any]) -> ToolUIContent? {
        guard let data = try? JSONSerialization.data(withJSONObject: args) else {
            return nil
        }

        let decoder = JSONDecoder()

        switch name {
        case "ui_dataTable":
            if let table = try? decoder.decode(ToolUIDataTable.self, from: data) {
                return .dataTable(table)
            }
        case "ui_optionList":
            if let list = try? decoder.decode(ToolUIOptionList.self, from: data) {
                return .optionList(list)
            }
        case "ui_progress":
            if let progress = try? decoder.decode(ToolUIProgress.self, from: data) {
                return .progress(progress)
            }
        case "ui_alert", "ui_notification":
            if let alert = try? decoder.decode(ToolUIAlert.self, from: data) {
                return .alert(alert)
            }
        case "ui_codeBlock", "ui_code":
            if let code = try? decoder.decode(ToolUICodeBlock.self, from: data) {
                return .codeBlock(code)
            }
        default:
            break
        }

        // Return unknown for any unrecognized UI tool
        if name.hasPrefix("ui_") {
            let argsString = String(data: data, encoding: .utf8) ?? "{}"
            return .unknown(name: name, args: argsString)
        }

        return nil
    }
}
