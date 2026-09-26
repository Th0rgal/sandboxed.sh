import { render } from "solid-js/web";
import App from "./App";
import {FindBar} from "./FindBar";
import { initTheme } from "./theme";
import "./styles.css";

initTheme();
render(() => <><App /><FindBar /></>, document.getElementById("root")!);
