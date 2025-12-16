"use client";

import { cn } from "@/lib/utils";

export interface DataTableColumn {
  id: string;
  label?: string;
  width?: string;
}

export interface DataTableRow {
  [key: string]: unknown;
}

export interface DataTableProps {
  id: string;
  title?: string;
  columns: DataTableColumn[];
  rows: DataTableRow[];
  className?: string;
}

export function DataTable({
  id,
  title,
  columns,
  rows,
  className,
}: DataTableProps) {
  return (
    <div
      className={cn(
        "w-full max-w-2xl overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02]",
        className
      )}
      data-slot="data-table"
      data-tool-ui-id={id}
    >
      {title && (
        <div className="border-b border-white/[0.06] px-4 py-3">
          <h3 className="text-sm font-medium text-white">{title}</h3>
        </div>
      )}
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-white/[0.06] bg-white/[0.02]">
              {columns.map((col) => (
                <th
                  key={col.id}
                  className="px-4 py-2.5 text-left text-[10px] font-medium uppercase tracking-wider text-white/40"
                  style={col.width ? { width: col.width } : undefined}
                >
                  {col.label ?? col.id}
                </th>
              ))}
            </tr>
          </thead>
          <tbody className="divide-y divide-white/[0.04]">
            {rows.length === 0 ? (
              <tr>
                <td
                  colSpan={columns.length}
                  className="px-4 py-8 text-center text-white/40"
                >
                  No data
                </td>
              </tr>
            ) : (
              rows.map((row, rowIndex) => (
                <tr key={rowIndex} className="hover:bg-white/[0.02] transition-colors">
                  {columns.map((col) => (
                    <td key={col.id} className="px-4 py-3 text-white/80">
                      {formatCellValue(row[col.id])}
                    </td>
                  ))}
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function formatCellValue(value: unknown): string {
  if (value === null || value === undefined) {
    return "-";
  }
  if (typeof value === "boolean") {
    return value ? "Yes" : "No";
  }
  if (typeof value === "number") {
    return value.toLocaleString();
  }
  if (typeof value === "object") {
    return JSON.stringify(value);
  }
  return String(value);
}

export interface SerializableDataTable {
  id: string;
  title?: string;
  columns: Array<{ id: string; label?: string; width?: string }>;
  rows: Array<Record<string, unknown>>;
}

export function parseSerializableDataTable(input: unknown): SerializableDataTable | null {
  if (!input || typeof input !== "object") return null;
  
  const obj = input as Record<string, unknown>;
  
  const id = typeof obj.id === "string" ? obj.id : `table-${Date.now()}`;
  
  if (!Array.isArray(obj.columns) || !Array.isArray(obj.rows)) {
    return null;
  }
  
  const columns: Array<{ id: string; label?: string; width?: string }> = [];
  
  for (const col of obj.columns) {
    if (typeof col === "string") {
      columns.push({ id: col, label: col });
    } else if (typeof col === "object" && col !== null) {
      const colObj = col as Record<string, unknown>;
      const colId = String(colObj.id ?? colObj.key ?? colObj.field ?? colObj.name ?? colObj.header ?? "col");
      const label = String(colObj.label ?? colObj.header ?? colObj.title ?? colObj.name ?? colId);
      columns.push({ 
        id: colId, 
        label, 
        width: typeof colObj.width === "string" ? colObj.width : undefined 
      });
    }
  }
  
  if (columns.length === 0) return null;
  
  return {
    id,
    title: typeof obj.title === "string" ? obj.title : undefined,
    columns,
    rows: obj.rows as Array<Record<string, unknown>>,
  };
}
