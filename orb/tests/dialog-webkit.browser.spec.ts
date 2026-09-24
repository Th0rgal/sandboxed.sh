import { test } from "@playwright/test";
import { dialogCases } from "./dialogCases";
test.use({ browserName: "webkit" });
dialogCases("webkit");
