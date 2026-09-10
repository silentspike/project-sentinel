import { render } from "solid-js/web";
import App from "./App";
import { CustomerWorkspace } from "./customer/CustomerWorkspace";

const root = document.getElementById("root");
if (root) render(() => new URLSearchParams(window.location.search).get("view") === "customer" ? <CustomerWorkspace /> : <App />, root);
