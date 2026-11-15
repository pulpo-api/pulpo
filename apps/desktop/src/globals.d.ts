export {};

declare module "vue" {
  interface ComponentCustomProperties {
    /** The text in the user's language (see i18n.ts). */
    $t: typeof import("./i18n").t;
  }
}
