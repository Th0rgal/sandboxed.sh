"use client";

import * as React from "react";
import { cn } from "@/lib/utils";

export interface ButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "default" | "destructive" | "outline" | "secondary" | "ghost" | "link";
  size?: "default" | "sm" | "lg" | "icon";
}

const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant = "default", size = "default", ...props }, ref) => {
    return (
      <button
        className={cn(
          "inline-flex items-center justify-center whitespace-nowrap rounded-lg text-sm font-medium transition-colors focus-visible:outline-none cursor-pointer disabled:pointer-events-none disabled:opacity-50 disabled:cursor-not-allowed",
          {
            // Primary (accent)
            "bg-indigo-500 hover:bg-indigo-600 text-white":
              variant === "default",
            // Destructive
            "bg-red-500 hover:bg-red-600 text-white":
              variant === "destructive",
            // Outline/Ghost subtle
            "bg-white/[0.04] hover:bg-white/[0.08] border border-white/[0.06] hover:border-white/[0.12] text-white/80":
              variant === "outline",
            // Secondary
            "bg-white/[0.06] hover:bg-white/[0.10] text-white/80":
              variant === "secondary",
            // Ghost
            "hover:bg-white/[0.04] text-white/60 hover:text-white/80":
              variant === "ghost",
            // Link
            "text-indigo-400 underline-offset-4 hover:underline":
              variant === "link",
          },
          {
            "h-9 px-4 py-2": size === "default",
            "h-8 rounded-md px-3 text-xs": size === "sm",
            "h-10 rounded-lg px-6": size === "lg",
            "h-9 w-9": size === "icon",
          },
          className
        )}
        ref={ref}
        {...props}
      />
    );
  }
);
Button.displayName = "Button";

export { Button };
