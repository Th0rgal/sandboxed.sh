//
//  ToolUIOptionListView.swift
//  SandboxedDashboard
//
//  SwiftUI renderer for ui_optionList tool
//

import SwiftUI

struct ToolUIOptionListView: View {
    let optionList: ToolUIOptionList
    let onSelect: ((String) -> Void)?
    
    @State private var selectedIds: Set<String> = []
    
    init(optionList: ToolUIOptionList, onSelect: ((String) -> Void)? = nil) {
        self.optionList = optionList
        self.onSelect = onSelect
        
        // Initialize with confirmed values if present
        let confirmed = optionList.confirmedIds
        _selectedIds = State(initialValue: Set(confirmed))
    }
    
    var isConfirmed: Bool {
        !optionList.confirmedIds.isEmpty
    }
    
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(optionList.options) { option in
                if isConfirmed {
                    // Only show confirmed options
                    if optionList.confirmedIds.contains(option.id) {
                        confirmedOptionRow(option)
                    }
                } else {
                    // Show all options as selectable
                    selectableOptionRow(option)
                }
            }
        }
        .padding(10)
        .background(Theme.backgroundSecondary.opacity(0.5))
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .stroke(Theme.border, lineWidth: 0.5)
        )
    }
    
    private func confirmedOptionRow(_ option: ToolUIOptionList.Option) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(Theme.success)
                .font(.title3)
            
            VStack(alignment: .leading, spacing: 2) {
                Text(option.label)
                    .font(.subheadline.weight(.medium))
                    .foregroundStyle(Theme.textPrimary)
                
                if let description = option.description {
                    Text(description)
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            
            Spacer()
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 12)
        .background(Theme.success.opacity(0.1))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
    
    private func selectableOptionRow(_ option: ToolUIOptionList.Option) -> some View {
        let isSelected = selectedIds.contains(option.id)
        let isDisabled = option.disabled ?? false
        
        return Button {
            guard !isDisabled else { return }
            
            if optionList.isSingleSelect {
                selectedIds = [option.id]
            } else {
                if selectedIds.contains(option.id) {
                    selectedIds.remove(option.id)
                } else {
                    selectedIds.insert(option.id)
                }
            }
            
            HapticService.selectionChanged()
            onSelect?(option.id)
        } label: {
            HStack(spacing: 12) {
                // Selection indicator
                Image(systemName: isSelected ? 
                      (optionList.isSingleSelect ? "circle.inset.filled" : "checkmark.square.fill") :
                      (optionList.isSingleSelect ? "circle" : "square"))
                    .foregroundStyle(isSelected ? Theme.accent : Theme.textMuted)
                    .font(.title3)
                
                VStack(alignment: .leading, spacing: 2) {
                    Text(option.label)
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(isDisabled ? Theme.textMuted : Theme.textPrimary)
                    
                    if let description = option.description {
                        Text(description)
                            .font(.caption)
                            .foregroundStyle(Theme.textTertiary)
                    }
                }
                
                Spacer()
            }
            .padding(.vertical, 10)
            .padding(.horizontal, 12)
            .background(isSelected ? Theme.accent.opacity(0.1) : Color.clear)
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .stroke(isSelected ? Theme.accent.opacity(0.3) : Theme.border.opacity(0.5), lineWidth: 1)
            )
        }
        .buttonStyle(.plain)
        .disabled(isDisabled)
        .opacity(isDisabled ? 0.5 : 1)
    }
}

#Preview {
    VStack(spacing: 20) {
        Text("Option List Preview")
    }
    .padding()
    .background(Theme.backgroundPrimary)
}
