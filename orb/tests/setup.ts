import { afterEach } from "vitest";
import { cleanup } from "@solidjs/testing-library";
afterEach(() => { cleanup(); sessionStorage.clear(); localStorage.clear(); });
