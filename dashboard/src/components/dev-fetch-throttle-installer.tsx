"use client";

import { useEffect } from "react";
import { installFetchThrottle } from "@/lib/dev-fetch-throttle";

export function DevFetchThrottleInstaller() {
  useEffect(() => {
    installFetchThrottle();
  }, []);
  return null;
}
