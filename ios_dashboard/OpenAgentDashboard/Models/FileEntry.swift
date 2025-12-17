//
//  FileEntry.swift
//  OpenAgentDashboard
//
//  File system entry models
//

import Foundation

struct FileEntry: Codable, Identifiable {
    let name: String
    let path: String
    let kind: String
    let size: Int
    let mtime: Int
    
    var id: String { path }
    
    var isDirectory: Bool {
        kind == "dir"
    }
    
    var isFile: Bool {
        kind == "file"
    }
    
    var icon: String {
        if isDirectory {
            return "folder.fill"
        }
        
        let ext = (name as NSString).pathExtension.lowercased()
        switch ext {
        case "swift", "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "cpp", "h":
            return "doc.text.fill"
        case "json", "yaml", "yml", "toml", "xml":
            return "doc.badge.gearshape.fill"
        case "md", "txt", "log":
            return "doc.plaintext.fill"
        case "png", "jpg", "jpeg", "gif", "svg", "webp":
            return "photo.fill"
        case "pdf":
            return "doc.richtext.fill"
        case "zip", "tar", "gz", "rar":
            return "doc.zipper"
        default:
            return "doc.fill"
        }
    }
    
    var formattedSize: String {
        guard isFile else { return "—" }
        if size < 1024 { return "\(size) B" }
        
        let units = ["KB", "MB", "GB", "TB"]
        var value = Double(size) / 1024.0
        var unitIndex = 0
        
        while value >= 1024 && unitIndex < units.count - 1 {
            value /= 1024.0
            unitIndex += 1
        }
        
        return value >= 10 ? String(format: "%.0f %@", value, units[unitIndex]) 
                          : String(format: "%.1f %@", value, units[unitIndex])
    }
    
    var modifiedDate: Date? {
        guard mtime > 0 else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(mtime))
    }
}

// Extend Date for relative formatting (shared across views)
extension Date {
    var relativeFormatted: String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(for: self, relativeTo: Date())
    }
}
