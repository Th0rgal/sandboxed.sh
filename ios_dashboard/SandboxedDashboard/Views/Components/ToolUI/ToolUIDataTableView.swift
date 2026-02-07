//
//  ToolUIDataTableView.swift
//  SandboxedDashboard
//
//  SwiftUI renderer for ui_dataTable tool
//

import SwiftUI

struct ToolUIDataTableView: View {
    let table: ToolUIDataTable
    
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Title header
            if let title = table.title {
                HStack(spacing: 8) {
                    Image(systemName: "tablecells")
                        .font(.caption)
                        .foregroundStyle(Theme.accent)
                    
                    Text(title)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.textPrimary)
                    
                    Spacer()
                    
                    // Row count badge
                    Text("\(table.rows.count) rows")
                        .font(.caption2)
                        .foregroundStyle(Theme.textTertiary)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
                .background(Theme.backgroundSecondary)
            }
            
            // Debug info if no columns
            if table.columns.isEmpty {
                Text("No columns defined")
                    .font(.caption)
                    .foregroundStyle(Theme.textMuted)
                    .padding()
            } else {
                // Table content with horizontal scroll
                ScrollView(.horizontal, showsIndicators: true) {
                    VStack(alignment: .leading, spacing: 0) {
                        // Header row
                        HStack(spacing: 0) {
                            ForEach(table.columns, id: \.id) { column in
                                Text(column.displayLabel)
                                    .font(.caption2.weight(.bold))
                                    .foregroundStyle(Theme.textMuted)
                                    .textCase(.uppercase)
                                    .lineLimit(2)
                                    .frame(width: columnWidth(for: column), alignment: .leading)
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 8)
                            }
                        }
                        .background(Color.white.opacity(0.03))
                        
                        Rectangle()
                            .fill(Theme.border)
                            .frame(height: 0.5)
                        
                        // Data rows
                        if table.rows.isEmpty {
                            Text("No data")
                                .font(.subheadline)
                                .foregroundStyle(Theme.textMuted)
                                .padding()
                        } else {
                            VStack(alignment: .leading, spacing: 0) {
                                ForEach(Array(table.rows.enumerated()), id: \.offset) { index, row in
                                    HStack(spacing: 0) {
                                        ForEach(Array(table.columns.enumerated()), id: \.element.id) { colIndex, column in
                                            let cellValue = getCellValue(row: row, columnId: column.id)
                                            
                                            Text(cellValue)
                                                .font(.caption)
                                                .foregroundStyle(colIndex == 0 ? Theme.textPrimary : Theme.textSecondary)
                                                .fontWeight(colIndex == 0 ? .medium : .regular)
                                                .lineLimit(3)
                                                .frame(width: columnWidth(for: column), alignment: .leading)
                                                .padding(.horizontal, 10)
                                                .padding(.vertical, 10)
                                        }
                                    }
                                    .background(index % 2 == 0 ? Color.clear : Color.white.opacity(0.02))
                                    
                                    if index < table.rows.count - 1 {
                                        Rectangle()
                                            .fill(Theme.border.opacity(0.3))
                                            .frame(height: 0.5)
                                    }
                                }
                            }
                        }
                    }
                    .frame(minWidth: totalTableWidth)
                }
            }
        }
        .background(Theme.backgroundSecondary.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }
    
    private func getCellValue(row: [String: AnyCodable], columnId: String) -> String {
        // Try exact match first
        if let value = row[columnId] {
            return value.stringValue
        }
        // Try case-insensitive match
        let lowerId = columnId.lowercased()
        for (key, value) in row {
            if key.lowercased() == lowerId {
                return value.stringValue
            }
        }
        return "-"
    }
    
    private var totalTableWidth: CGFloat {
        table.columns.reduce(0) { $0 + columnWidth(for: $1) + 20 } // 20 for padding
    }
    
    private func columnWidth(for column: ToolUIDataTable.Column) -> CGFloat {
        // Parse width if provided, otherwise use adaptive default
        if let width = column.width {
            if width.hasSuffix("px") {
                let numStr = width.dropLast(2)
                if let num = Double(numStr) {
                    return min(200, max(80, CGFloat(num)))
                }
            }
            if let num = Double(width) {
                return min(200, max(80, CGFloat(num)))
            }
        }
        // Smart default based on column id
        let id = column.id.lowercased()
        if id.contains("name") || id.contains("model") || id.contains("description") {
            return 140
        } else if id.contains("id") || id.contains("cost") || id.contains("price") {
            return 90
        }
        return 110
    }
}

#Preview {
    VStack {
        // Preview would go here
        Text("Data Table Preview")
    }
    .padding()
    .background(Theme.backgroundPrimary)
}
