import { describe, it, expect } from "vitest";
import { render } from "@solidjs/testing-library";
import { GaiaWizardView } from "../src/views/GaiaWizardView";

describe("GaiaWizardView (#421)", () => {
  it("renders step 1 with the company form", () => {
    const { getByTestId } = render(GaiaWizardView);
    expect(getByTestId("view-gaia-wizard")).toBeTruthy();
    expect(getByTestId("gw-company-name")).toBeTruthy();
  });

  it("disables Next on step 1 until a non-empty company name is entered (reactive validation)", () => {
    const { getByTestId } = render(GaiaWizardView);
    const next = getByTestId("gw-next") as HTMLButtonElement;
    expect(next.disabled).toBe(true); // empty company_name -> step invalid

    const input = getByTestId("gw-company-name") as HTMLInputElement;
    input.value = "Acme Corp";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    expect(next.disabled).toBe(false); // name + default agent_count>=1 -> valid
  });
});
