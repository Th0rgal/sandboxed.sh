import { useMDXComponents as getDocsMDXComponents } from "nextra-theme-docs";
import type { ReactNode } from "react";

const docsComponents = getDocsMDXComponents();

// Custom feature card component
function FeatureCard({
  title,
  children,
  icon,
}: {
  title: string;
  children: ReactNode;
  icon?: string;
}) {
  return (
    <div
      style={{
        padding: "1.25rem",
        borderRadius: "0.75rem",
        backgroundColor: "rgba(255, 248, 240, 0.02)",
        border: "1px solid rgba(255, 248, 240, 0.06)",
        marginBottom: "1rem",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: "0.5rem", marginBottom: "0.5rem" }}>
        {icon && <span style={{ fontSize: "1.25rem" }}>{icon}</span>}
        <h4 style={{ margin: 0, fontSize: "0.875rem", fontWeight: 600, color: "rgb(245, 240, 235)" }}>
          {title}
        </h4>
      </div>
      <div style={{ color: "rgba(245, 240, 235, 0.6)", fontSize: "0.875rem" }}>{children}</div>
    </div>
  );
}

// Custom badge component with blue accent
function Badge({
  children,
  variant = "default",
}: {
  children: ReactNode;
  variant?: "default" | "success" | "warning" | "error";
}) {
  const colors = {
    default: { bg: "rgba(59, 130, 246, 0.1)", text: "rgb(59, 130, 246)" },
    success: { bg: "rgba(124, 207, 155, 0.1)", text: "rgb(124, 207, 155)" },
    warning: { bg: "rgba(244, 178, 127, 0.1)", text: "rgb(244, 178, 127)" },
    error: { bg: "rgba(248, 113, 113, 0.1)", text: "rgb(248, 113, 113)" },
  };
  const { bg, text } = colors[variant];

  return (
    <span
      style={{
        display: "inline-flex",
        padding: "0.25rem 0.625rem",
        borderRadius: "0.375rem",
        fontSize: "0.625rem",
        fontWeight: 600,
        textTransform: "uppercase",
        letterSpacing: "0.05em",
        backgroundColor: bg,
        color: text,
      }}
    >
      {children}
    </span>
  );
}

export const useMDXComponents = (components?: Record<string, unknown>) => ({
  ...docsComponents,
  FeatureCard,
  Badge,
  ...(components || {}),
});
