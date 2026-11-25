// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { FluentBundle, FluentResource } from "../vendor/fluent-bundle/index.js";
import { CATALOGS } from "./catalogs.mjs";

export function cardinal(val) { return { type: "cardinal", value: val }; }
export function integer(val) { return { type: "integer", value: val }; }

const LOCALES = {
  "en-US": { bundleLocale: "en-US", cardinal: "en" },
};

const CATEGORIES = {
  "en": (n) => (n == 1) ? "one" : "other",
};

const MESSAGES = {
  "windows.about.app": {
    resourceKey: "m77696e646f77732e61626f75742e617070",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.about.dayago": {
    resourceKey: "m77696e646f77732e61626f75742e64617961676f",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.hourago": {
    resourceKey: "m77696e646f77732e61626f75742e686f757261676f",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.journal": {
    resourceKey: "m77696e646f77732e61626f75742e6a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.about.justnow": {
    resourceKey: "m77696e646f77732e61626f75742e6a7573746e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.about.lastseen": {
    resourceKey: "m77696e646f77732e61626f75742e6c6173747365656e",
    formatIntent: "text",
    arguments: [
      { name: "line", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "age", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line0": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6530",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line1": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6531",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "build", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line2": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6532",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "platform", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line3": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6533",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "build", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "platform", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line4": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6534",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arch", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line5": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6535",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "build", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arch", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line6": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6536",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "platform", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arch", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.line7": {
    resourceKey: "m77696e646f77732e61626f75742e6c696e6537",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "version", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "build", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "platform", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arch", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.minuteago": {
    resourceKey: "m77696e646f77732e61626f75742e6d696e75746561676f",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.about.unknown": {
    resourceKey: "m77696e646f77732e61626f75742e756e6b6e6f776e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.browser.discarded": {
    resourceKey: "m77696e646f77732e62726f777365722e646973636172646564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.about.journalunknown": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e61626f75742e6a6f75726e616c756e6b6e6f776e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.about.opensourcelocalfirst": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e61626f75742e6f70656e736f757263656c6f63616c6669727374",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.about.pendingsettingstab": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e61626f75742e70656e64696e6773657474696e6773746162",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.about.setupwelcomebody1": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e61626f75742e736574757077656c636f6d65626f647931",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.about.textcontent": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e61626f75742e74657874636f6e74656e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.about.textcontent2": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e61626f75742e74657874636f6e74656e7432",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.aboutsolstone": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e61626f7574736f6c73746f6e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.emptytext": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e656d70747974657874",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.frontendorigin": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e66726f6e74656e646f726967696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.justnow": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e6a7573746e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.microphonemodeoff": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e6d6963726f70686f6e656d6f64656f6666",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.notpaired": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e6e6f74706169726564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.offlinetitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e6f66666c696e657469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.settingsdiagnosticson": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e73657474696e6773646961676e6f73746963736f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.app.sourcestitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e6170702e736f75726365737469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.audiosourcesystem": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e617564696f736f7572636573797374656d",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.checkagain": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e636865636b616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.choosedevice": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e63686f6f7365646576696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.decidingtitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6465636964696e677469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.decisionrefusedbody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6465636973696f6e72656675736564626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.decisionrefusedtitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6465636973696f6e726566757365647469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.decisionunknownbody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6465636973696f6e756e6b6e6f776e626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.decisionunknowntitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6465636973696f6e756e6b6e6f776e7469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.deferchoice": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e646566657263686f696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.journalmarkconfirmbutton": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6a6f75726e616c6d61726b636f6e6669726d627574746f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.journalmarkconfirmquestion": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6a6f75726e616c6d61726b636f6e6669726d7175657374696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.journalmarkmismatchbutton": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6a6f75726e616c6d61726b6d69736d61746368627574746f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.journalmarkunverifiedcancelbutton": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6a6f75726e616c6d61726b756e766572696669656463616e63656c627574746f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.journalmarkunverifiedcontinuebutton": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6a6f75726e616c6d61726b756e7665726966696564636f6e74696e7565627574746f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.journalmarkunverifiedtitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6a6f75726e616c6d61726b756e76657269666965647469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.keepboth": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6b656570626f7468",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.listunavailablebody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6c697374756e617661696c61626c65626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.listunavailabletitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6c697374756e617661696c61626c657469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.offlinebody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e6f66666c696e65626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.pickerempty": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7069636b6572656d707479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.replaceconfirmbody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7265706c616365636f6e6669726d626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.replaceconfirmtitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7265706c616365636f6e6669726d7469746c65",
    formatIntent: "text",
    arguments: [
      { name: "selecteddevicelabel", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.desktop.shared.copy.replaceconfirmtitlefallback": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7265706c616365636f6e6669726d7469746c6566616c6c6261636b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.replacedevice": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7265706c616365646576696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.replaceofferbody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7265706c6163656f66666572626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.replaceoffertitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7265706c6163656f666665727469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.settingsjournalopen": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657474696e67736a6f75726e616c6f70656e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.settingslogexportsave": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657474696e67736c6f676578706f727473617665",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.settingslogexporttitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657474696e67736c6f676578706f72747469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.settingstabattentiona11y": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657474696e6773746162617474656e74696f6e61313179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.settingstabupdatesdonea11y": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657474696e677374616275706461746573646f6e6561313179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupapplabel": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657475706170706c6162656c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupback": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657475706261636b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupconnectedtoyourjournal": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7365747570636f6e6e6563746564746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupcontinue": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7365747570636f6e74696e7565",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupjournalcardheading": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657475706a6f75726e616c6361726468656164696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupmicrophoneheadline": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657475706d6963726f70686f6e65686561646c696e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupopenyourjournal": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73657475706f70656e796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupscreenframecaption": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736574757073637265656e6672616d6563617074696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupwelcomebody2": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736574757077656c636f6d65626f647932",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.setupwindowtitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736574757077696e646f777469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.sourcesbrowserconnectednow": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736f757263657362726f77736572636f6e6e65637465646e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.sourcesbrowserdiscard": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736f757263657362726f7773657264697363617264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.sourcesbrowserdiscardcommit": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736f757263657362726f7773657264697363617264636f6d6d6974",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.sourcesbrowserdiscardconfirm": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736f757263657362726f7773657264697363617264636f6e6669726d",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.sourcesbrowserheld": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736f757263657362726f7773657268656c64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.sourcesbrowserpages": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e736f757263657362726f777365727061676573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.storageunavailablebody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73746f72616765756e617661696c61626c65626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.storageunavailabletitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e73746f72616765756e617661696c61626c657469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.targetmissingbody": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7461726765746d697373696e67626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.targetmissingtitle": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e7461726765746d697373696e677469746c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.technicaldetails": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e746563686e6963616c64657461696c73",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.copy.text": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e74657874",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "value2", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.desktop.shared.copy.tryagain": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e636f70792e747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.add": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e616464",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.excludedapps": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e6578636c7564656461707073",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.gethelp": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e67657468656c70",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.help": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e68656c70",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.reportaproblem": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e7265706f72746170726f626c656d",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.severity": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e7365766572697479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.textcontent": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e74657874636f6e74656e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.updates": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e75706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.settings.windowtitles": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657474696e67732e77696e646f777469746c6573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.setup.cancel": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657475702e63616e63656c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.setup.pair": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657475702e70616972",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.setup.startatlogin": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657475702e737461727461746c6f67696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.setup.wizardpausefailure": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657475702e77697a61726470617573656661696c757265",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.setup.wizardpausefailure2": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e73657475702e77697a61726470617573656661696c75726532",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.automaticupdates": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e6175746f6d6174696375706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.checkforupdatesautomatically": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e636865636b666f72757064617465736175746f6d61746963616c6c79",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.checknow": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e636865636b6e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.dismiss": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e6469736d697373",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.download": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e646f776e6c6f6164",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.downloadupdatesinthebackground": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e646f776e6c6f616475706461746573696e7468656261636b67726f756e64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.everyday": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e6576657279646179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.everymonth": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e65766572796d6f6e7468",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.everyweek": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e65766572797765656b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.howoften": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e686f776f6674656e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.nevercheckedforupdates": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e6e65766572636865636b6564666f7275706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.textcontent": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e74657874636f6e74656e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.desktop.shared.updates.whatsnew": {
    resourceKey: "m77696e646f77732e6465736b746f702e7368617265642e757064617465732e77686174736e6577",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.exclusions.activity": {
    resourceKey: "m77696e646f77732e6578636c7573696f6e732e6163746976697479",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.exclusions.activityredacted": {
    resourceKey: "m77696e646f77732e6578636c7573696f6e732e61637469766974797265646163746564",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "more", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.health.attentioncount": {
    resourceKey: "m77696e646f77732e6865616c74682e617474656e74696f6e636f756e74",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.allhistory": {
    resourceKey: "m77696e646f77732e696d706f72742e616c6c686973746f7279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.cancelaction": {
    resourceKey: "m77696e646f77732e696d706f72742e63616e63656c616374696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.cancelactiona11y": {
    resourceKey: "m77696e646f77732e696d706f72742e63616e63656c616374696f6e61313179",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.errorchecksum": {
    resourceKey: "m77696e646f77732e696d706f72742e6572726f72636865636b73756d",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.errorexpired": {
    resourceKey: "m77696e646f77732e696d706f72742e6572726f7265787069726564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.errornospace": {
    resourceKey: "m77696e646f77732e696d706f72742e6572726f726e6f7370616365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.errorunreadable": {
    resourceKey: "m77696e646f77732e696d706f72742e6572726f72756e7265616461626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.expiredword": {
    resourceKey: "m77696e646f77732e696d706f72742e65787069726564776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.holdoffline": {
    resourceKey: "m77696e646f77732e696d706f72742e686f6c646f66666c696e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.intro": {
    resourceKey: "m77696e646f77732e696d706f72742e696e74726f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.morehelper": {
    resourceKey: "m77696e646f77732e696d706f72742e6d6f726568656c706572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.readytoreviewword": {
    resourceKey: "m77696e646f77732e696d706f72742e7265616479746f726576696577776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.recenta11y": {
    resourceKey: "m77696e646f77732e696d706f72742e726563656e7461313179",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "relative", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.recentmeta": {
    resourceKey: "m77696e646f77732e696d706f72742e726563656e746d657461",
    formatIntent: "text",
    arguments: [
      { name: "relative", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.removereadybody": {
    resourceKey: "m77696e646f77732e696d706f72742e72656d6f76657265616479626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.reviewaction": {
    resourceKey: "m77696e646f77732e696d706f72742e726576696577616374696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.reviewactiona11y": {
    resourceKey: "m77696e646f77732e696d706f72742e726576696577616374696f6e61313179",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.reviewdetail": {
    resourceKey: "m77696e646f77732e696d706f72742e72657669657764657461696c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.reviewdetailbody": {
    resourceKey: "m77696e646f77732e696d706f72742e72657669657764657461696c626f6479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.reviewexpired": {
    resourceKey: "m77696e646f77732e696d706f72742e72657669657765787069726564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.rowchecking": {
    resourceKey: "m77696e646f77732e696d706f72742e726f77636865636b696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.rowpaused": {
    resourceKey: "m77696e646f77732e696d706f72742e726f77706175736564",
    formatIntent: "text",
    arguments: [
      { name: "sent", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "total", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.rowprogress": {
    resourceKey: "m77696e646f77732e696d706f72742e726f7770726f6772657373",
    formatIntent: "text",
    arguments: [
      { name: "sent", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "total", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "percent", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.rowsending": {
    resourceKey: "m77696e646f77732e696d706f72742e726f7773656e64696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.sectionreview": {
    resourceKey: "m77696e646f77732e696d706f72742e73656374696f6e726576696577",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.sendagainaction": {
    resourceKey: "m77696e646f77732e696d706f72742e73656e64616761696e616374696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.sendagainactiona11y": {
    resourceKey: "m77696e646f77732e696d706f72742e73656e64616761696e616374696f6e61313179",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.statusattentionheadline": {
    resourceKey: "m77696e646f77732e696d706f72742e737461747573617474656e74696f6e686561646c696e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.statusattentionsub": {
    resourceKey: "m77696e646f77732e696d706f72742e737461747573617474656e74696f6e737562",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.statusreviewheadline": {
    resourceKey: "m77696e646f77732e696d706f72742e737461747573726576696577686561646c696e65",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.statusreviewsub": {
    resourceKey: "m77696e646f77732e696d706f72742e737461747573726576696577737562",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.tilea11y": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c6561313179",
    formatIntent: "text",
    arguments: [
      { name: "state", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "sub", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.tilechecking": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c65636865636b696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.import.tilereadycount": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c657265616479636f756e74",
    formatIntent: "text",
    arguments: [
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.tilesendingbytes": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c6573656e64696e676279746573",
    formatIntent: "text",
    arguments: [
      { name: "sent", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "total", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.tilesendingbytesmore": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c6573656e64696e6762797465736d6f7265",
    formatIntent: "text",
    arguments: [
      { name: "sent", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "total", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.tilesendingcount": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c6573656e64696e67636f756e74",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.tilewaitingcount": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c6577616974696e67636f756e74",
    formatIntent: "text",
    arguments: [
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.import.tilewaitingjournal": {
    resourceKey: "m77696e646f77732e696d706f72742e74696c6577616974696e676a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.availablelogsareshownbelowcheckthe": {
    resourceKey: "m77696e646f77732e6c6f67732e617661696c61626c656c6f677361726573686f776e62656c6f77636865636b746865",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.copysaved": {
    resourceKey: "m77696e646f77732e6c6f67732e636f70797361766564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.couldntreadthelogfilestry": {
    resourceKey: "m77696e646f77732e6c6f67732e636f756c646e74726561647468656c6f6766696c6573747279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.nologfilesareavailableyettry": {
    resourceKey: "m77696e646f77732e6c6f67732e6e6f6c6f6766696c6573617265617661696c61626c65796574747279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.norecentlogsareavailableyettry": {
    resourceKey: "m77696e646f77732e6c6f67732e6e6f726563656e746c6f6773617265617661696c61626c65796574747279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.readthelogsbelowthenchoosewhether": {
    resourceKey: "m77696e646f77732e6c6f67732e726561647468656c6f677362656c6f777468656e63686f6f736577686574686572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.textcontent": {
    resourceKey: "m77696e646f77732e6c6f67732e74657874636f6e74656e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.textcontent2": {
    resourceKey: "m77696e646f77732e6c6f67732e74657874636f6e74656e7432",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.textcontent3": {
    resourceKey: "m77696e646f77732e6c6f67732e74657874636f6e74656e7433",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.textcontent4": {
    resourceKey: "m77696e646f77732e6c6f67732e74657874636f6e74656e7434",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.logs.textcontent5": {
    resourceKey: "m77696e646f77732e6c6f67732e74657874636f6e74656e7435",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.everythingsavedonthispcisgoing": {
    resourceKey: "m77696e646f77732e6e6f746963652e65766572797468696e6773617665646f6e7468697370636973676f696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.everythingyouturnedonisgoinginto": {
    resourceKey: "m77696e646f77732e6e6f746963652e65766572797468696e67796f757475726e65646f6e6973676f696e67696e746f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.intakeisrunningagain": {
    resourceKey: "m77696e646f77732e6e6f746963652e696e74616b65697372756e6e696e67616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.intakeranintoaproblem": {
    resourceKey: "m77696e646f77732e6e6f746963652e696e74616b6572616e696e746f6170726f626c656d",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.itsgoingintoyourjournalagain": {
    resourceKey: "m77696e646f77732e6e6f746963652e697473676f696e67696e746f796f75726a6f75726e616c616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.solstoneisstilloneverythingissaved": {
    resourceKey: "m77696e646f77732e6e6f746963652e736f6c73746f6e6569737374696c6c6f6e65766572797468696e6769737361766564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.theisback": {
    resourceKey: "m77696e646f77732e6e6f746963652e74686569736261636b",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.notice.theneedsyourpermission": {
    resourceKey: "m77696e646f77732e6e6f746963652e7468656e65656473796f75727065726d697373696f6e",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.notice.thesolstoneiconinthetaskbarsays": {
    resourceKey: "m77696e646f77732e6e6f746963652e746865736f6c73746f6e6569636f6e696e7468657461736b62617273617973",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.windowsprivacysettingsareblockingitturn": {
    resourceKey: "m77696e646f77732e6e6f746963652e77696e646f77737072697661637973657474696e6773617265626c6f636b696e6769747475726e",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.notice.yourjournalisbackinreach": {
    resourceKey: "m77696e646f77732e6e6f746963652e796f75726a6f75726e616c69736261636b696e7265616368",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.notice.yourjournalisoutofreach": {
    resourceKey: "m77696e646f77732e6e6f746963652e796f75726a6f75726e616c69736f75746f667265616368",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.obsidian.chooseobsidianvault": {
    resourceKey: "m77696e646f77732e6f6273696469616e2e63686f6f73656f6273696469616e7661756c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.obsidian.vault": {
    resourceKey: "m77696e646f77732e6f6273696469616e2e7661756c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.obsidian.warning": {
    resourceKey: "m77696e646f77732e6f6273696469616e2e7761726e696e67",
    formatIntent: "text",
    arguments: [
      { name: "vault", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.obsidian.warningdate": {
    resourceKey: "m77696e646f77732e6f6273696469616e2e7761726e696e6764617465",
    formatIntent: "text",
    arguments: [
      { name: "vault", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "date", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.obsidian.warningdevice": {
    resourceKey: "m77696e646f77732e6f6273696469616e2e7761726e696e67646576696365",
    formatIntent: "text",
    arguments: [
      { name: "vault", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "device", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.obsidian.warningdevicedate": {
    resourceKey: "m77696e646f77732e6f6273696469616e2e7761726e696e6764657669636564617465",
    formatIntent: "text",
    arguments: [
      { name: "vault", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "device", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "date", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.pill.held": {
    resourceKey: "m77696e646f77732e70696c6c2e68656c64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.pill.heldwaiting": {
    resourceKey: "m77696e646f77732e70696c6c2e68656c6477616974696e67",
    formatIntent: "text",
    arguments: [
      { name: "waiting", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.pill.offline": {
    resourceKey: "m77696e646f77732e70696c6c2e6f66666c696e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.pill.offlinewaiting": {
    resourceKey: "m77696e646f77732e70696c6c2e6f66666c696e6577616974696e67",
    formatIntent: "text",
    arguments: [
      { name: "waiting", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.pill.readytoreview": {
    resourceKey: "m77696e646f77732e70696c6c2e7265616479746f726576696577",
    formatIntent: "text",
    arguments: [
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.pill.refused": {
    resourceKey: "m77696e646f77732e70696c6c2e72656675736564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.pill.refusedwaiting": {
    resourceKey: "m77696e646f77732e70696c6c2e7265667573656477616974696e67",
    formatIntent: "text",
    arguments: [
      { name: "waiting", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.pill.syncpaused": {
    resourceKey: "m77696e646f77732e70696c6c2e73796e63706175736564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.pill.syncpausedwaiting": {
    resourceKey: "m77696e646f77732e70696c6c2e73796e6370617573656477616974696e67",
    formatIntent: "text",
    arguments: [
      { name: "waiting", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.quantity.hours": {
    resourceKey: "m77696e646f77732e7175616e746974792e686f757273",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.quantity.items": {
    resourceKey: "m77696e646f77732e7175616e746974792e6974656d73",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.quantity.minutes": {
    resourceKey: "m77696e646f77732e7175616e746974792e6d696e75746573",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.quantity.notes": {
    resourceKey: "m77696e646f77732e7175616e746974792e6e6f746573",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.quantity.pages": {
    resourceKey: "m77696e646f77732e7175616e746974792e7061676573",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.quantity.screenshots": {
    resourceKey: "m77696e646f77732e7175616e746974792e73637265656e73686f7473",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.screenshots.bringcount": {
    resourceKey: "m77696e646f77732e73637265656e73686f74732e6272696e67636f756e74",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.screenshots.cantcount": {
    resourceKey: "m77696e646f77732e73637265656e73686f74732e63616e74636f756e74",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.screenshots.unreadablecount": {
    resourceKey: "m77696e646f77732e73637265656e73686f74732e756e7265616461626c65636f756e74",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.sharedsources.also": {
    resourceKey: "m77696e646f77732e736861726564736f75726365732e616c736f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.sharedsources.labelage": {
    resourceKey: "m77696e646f77732e736861726564736f75726365732e6c6162656c616765",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "relative", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.sharedsources.labelstate": {
    resourceKey: "m77696e646f77732e736861726564736f75726365732e6c6162656c7374617465",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "state", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.sharedsources.labelstateage": {
    resourceKey: "m77696e646f77732e736861726564736f75726365732e6c6162656c7374617465616765",
    formatIntent: "text",
    arguments: [
      { name: "label", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "state", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "relative", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.sharedsources.many": {
    resourceKey: "m77696e646f77732e736861726564736f75726365732e6d616e79",
    formatIntent: "text",
    arguments: [
      { name: "count", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.sharedsources.one": {
    resourceKey: "m77696e646f77732e736861726564736f75726365732e6f6e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.sharedsources.stateage": {
    resourceKey: "m77696e646f77732e736861726564736f75726365732e7374617465616765",
    formatIntent: "text",
    arguments: [
      { name: "state", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "relative", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.time.checkedday": {
    resourceKey: "m77696e646f77732e74696d652e636865636b6564646179",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.time.checkedhour": {
    resourceKey: "m77696e646f77732e74696d652e636865636b6564686f7572",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.time.checkedminute": {
    resourceKey: "m77696e646f77732e74696d652e636865636b65646d696e757465",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.time.nextsave": {
    resourceKey: "m77696e646f77732e74696d652e6e65787473617665",
    formatIntent: "text",
    arguments: [
      { name: "minutes", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "seconds", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.time.takenhours": {
    resourceKey: "m77696e646f77732e74696d652e74616b656e686f757273",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "display", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.checkforupdates": {
    resourceKey: "m77696e646f77732e747261792e636865636b666f7275706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.hrmin": {
    resourceKey: "m77696e646f77732e747261792e68726d696e",
    formatIntent: "text",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "count2", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.lessthanaminute": {
    resourceKey: "m77696e646f77732e747261792e6c6573737468616e616d696e757465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.needsarestart": {
    resourceKey: "m77696e646f77732e747261792e6e656564736172657374617274",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.onbut": {
    resourceKey: "m77696e646f77732e747261792e6f6e627574",
    formatIntent: "text",
    arguments: [
      { name: "needs", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.onconnectedtoyourjournal": {
    resourceKey: "m77696e646f77732e747261792e6f6e636f6e6e6563746564746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.onnotconnectedtoajournal": {
    resourceKey: "m77696e646f77732e747261792e6f6e6e6f74636f6e6e6563746564746f616a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.onsavedonthispc": {
    resourceKey: "m77696e646f77732e747261792e6f6e73617665646f6e746869737063",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.pausedleft": {
    resourceKey: "m77696e646f77732e747261792e7061757365646c656674",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.solstonewaitingforyoutoconfirmyour": {
    resourceKey: "m77696e646f77732e747261792e736f6c73746f6e6577616974696e67666f72796f75746f636f6e6669726d796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.starting": {
    resourceKey: "m77696e646f77732e747261792e7374617274696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.storagefaultforowner": {
    resourceKey: "m77696e646f77732e747261792e73746f726167656661756c74666f726f776e6572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.text": {
    resourceKey: "m77696e646f77732e747261792e74657874",
    formatIntent: "text",
    arguments: [
      { name: "tooltip", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "notice", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.theneedspermissioninwindowssettings": {
    resourceKey: "m77696e646f77732e747261792e7468656e656564737065726d697373696f6e696e77696e646f777373657474696e6773",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.thestoppedgoingintoyourjournal": {
    resourceKey: "m77696e646f77732e747261792e74686573746f70706564676f696e67696e746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.unknownjournalseenat": {
    resourceKey: "m77696e646f77732e747261792e756e6b6e6f776e6a6f75726e616c7365656e6174",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.tray.unknownjournalseenthroughtherelay": {
    resourceKey: "m77696e646f77732e747261792e756e6b6e6f776e6a6f75726e616c7365656e7468726f75676874686572656c6179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.updateavailable": {
    resourceKey: "m77696e646f77732e747261792e757064617465617661696c61626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.updatecheckfailed": {
    resourceKey: "m77696e646f77732e747261792e757064617465636865636b6661696c6564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.tray.updateready": {
    resourceKey: "m77696e646f77732e747261792e7570646174657265616479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.accesswasdenied": {
    resourceKey: "m77696e646f77732e75692e61636365737377617364656e696564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.accesswasdeniedcheckwindowspermissionsfor": {
    resourceKey: "m77696e646f77732e75692e61636365737377617364656e696564636865636b77696e646f77737065726d697373696f6e73666f72",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.activex474d1b75": {
    resourceKey: "m77696e646f77732e75692e616374697665783437346431623735",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.activex68f50a9b": {
    resourceKey: "m77696e646f77732e75692e616374697665783638663530613962",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.activex96879611": {
    resourceKey: "m77696e646f77732e75692e616374697665783936383739363131",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.activity": {
    resourceKey: "m77696e646f77732e75692e6163746976697479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.activityisstillon": {
    resourceKey: "m77696e646f77732e75692e616374697669747969737374696c6c6f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.addappnameswindowtitlesandpage": {
    resourceKey: "m77696e646f77732e75692e6164646170706e616d657377696e646f777469746c6573616e6470616765",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.addmore": {
    resourceKey: "m77696e646f77732e75692e6164646d6f7265",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.addspdfsaudioimagesandtextyouchoosehereordropont": {
    resourceKey: "m77696e646f77732e75692e6164647370646673617564696f696d61676573616e6474657874796f7563686f6f7365686572656f7264726f706f6e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.addthesolstoneextension": {
    resourceKey: "m77696e646f77732e75692e616464746865736f6c73746f6e65657874656e73696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.addto": {
    resourceKey: "m77696e646f77732e75692e616464746f",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.aglobalshortcuttopauseandresume": {
    resourceKey: "m77696e646f77732e75692e61676c6f62616c73686f7274637574746f7061757365616e64726573756d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.allcaughtup": {
    resourceKey: "m77696e646f77732e75692e616c6c6361756768747570",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.allofit": {
    resourceKey: "m77696e646f77732e75692e616c6c6f666974",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.allsourceson": {
    resourceKey: "m77696e646f77732e75692e616c6c736f75726365736f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.alouderinputforquietmicrophoneschanges": {
    resourceKey: "m77696e646f77732e75692e616c6f75646572696e707574666f7271756965746d6963726f70686f6e65736368616e676573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.alreadyonhome": {
    resourceKey: "m77696e646f77732e75692e616c72656164796f6e686f6d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.anewlinkfromyourjournalcanbepastedhere": {
    resourceKey: "m77696e646f77732e75692e616e65776c696e6b66726f6d796f75726a6f75726e616c63616e626570617374656468657265",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.anotherpairinglinkcouldnotbeadded": {
    resourceKey: "m77696e646f77732e75692e616e6f7468657270616972696e676c696e6b636f756c646e6f7462656164646564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.april": {
    resourceKey: "m77696e646f77732e75692e617072696c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.arg0": {
    resourceKey: "m77696e646f77732e75692e61726730",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0arg1": {
    resourceKey: "m77696e646f77732e75692e6172673061726731",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg1", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0arg1arg2": {
    resourceKey: "m77696e646f77732e75692e617267306172673161726732",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg1", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg2", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0bytes": {
    resourceKey: "m77696e646f77732e75692e617267306279746573",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0gb": {
    resourceKey: "m77696e646f77732e75692e617267306762",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0hr": {
    resourceKey: "m77696e646f77732e75692e617267306872",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0hrleft": {
    resourceKey: "m77696e646f77732e75692e6172673068726c656674",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0kb": {
    resourceKey: "m77696e646f77732e75692e617267306b62",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0kbx75b11ba1": {
    resourceKey: "m77696e646f77732e75692e617267306b62783735623131626131",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0mb": {
    resourceKey: "m77696e646f77732e75692e617267306d62",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0mbxff882f00": {
    resourceKey: "m77696e646f77732e75692e617267306d62786666383832663030",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0min": {
    resourceKey: "m77696e646f77732e75692e617267306d696e",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0minago": {
    resourceKey: "m77696e646f77732e75692e617267306d696e61676f",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0minleft": {
    resourceKey: "m77696e646f77732e75692e617267306d696e6c656674",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0needsattention": {
    resourceKey: "m77696e646f77732e75692e617267306e65656473617474656e74696f6e",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0ofarg1inyourjournal": {
    resourceKey: "m77696e646f77732e75692e617267306f6661726731696e796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg1", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0ofarg1screenshotsinyourjournal": {
    resourceKey: "m77696e646f77732e75692e617267306f666172673173637265656e73686f7473696e796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg1", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0tb": {
    resourceKey: "m77696e646f77732e75692e617267307462",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.arg0x211b8ba1": {
    resourceKey: "m77696e646f77732e75692e61726730783231316238626131",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.ascreenshotalreadyinyourjournalstaysthereifyoude": {
    resourceKey: "m77696e646f77732e75692e6173637265656e73686f74616c7265616479696e796f75726a6f75726e616c737461797374686572656966796f756465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.ascreenshotthatsonlycopiedtotheclipboardisntsave": {
    resourceKey: "m77696e646f77732e75692e6173637265656e73686f7474686174736f6e6c79636f70696564746f746865636c6970626f61726469736e7473617665",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.asegmentisa5minutelocal": {
    resourceKey: "m77696e646f77732e75692e617365676d656e74697361356d696e7574656c6f63616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.attentionneeded": {
    resourceKey: "m77696e646f77732e75692e617474656e74696f6e6e6565646564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.audio": {
    resourceKey: "m77696e646f77732e75692e617564696f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.august": {
    resourceKey: "m77696e646f77732e75692e617567757374",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.automaticchecksareoff": {
    resourceKey: "m77696e646f77732e75692e6175746f6d61746963636865636b736172656f6666",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.automaticchecksareon": {
    resourceKey: "m77696e646f77732e75692e6175746f6d61746963636865636b736172656f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.awindowthatclosesormovescan": {
    resourceKey: "m77696e646f77732e75692e6177696e646f7774686174636c6f7365736f726d6f76657363616e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.backgrounddatauserestricted": {
    resourceKey: "m77696e646f77732e75692e6261636b67726f756e646461746175736572657374726963746564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.browsers": {
    resourceKey: "m77696e646f77732e75692e62726f7773657273",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.browserstateunavailable": {
    resourceKey: "m77696e646f77732e75692e62726f777365727374617465756e617661696c61626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.change": {
    resourceKey: "m77696e646f77732e75692e6368616e6765",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.checkedjustnow": {
    resourceKey: "m77696e646f77732e75692e636865636b65646a7573746e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.checkforupdates": {
    resourceKey: "m77696e646f77732e75692e636865636b666f7275706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.checking": {
    resourceKey: "m77696e646f77732e75692e636865636b696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.checkingforupdates": {
    resourceKey: "m77696e646f77732e75692e636865636b696e67666f7275706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.checkingword": {
    resourceKey: "m77696e646f77732e75692e636865636b696e67776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.checkingyourjournal": {
    resourceKey: "m77696e646f77732e75692e636865636b696e67796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.chooseafolder": {
    resourceKey: "m77696e646f77732e75692e63686f6f736561666f6c646572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.chooseafolderxdbec742d": {
    resourceKey: "m77696e646f77732e75692e63686f6f736561666f6c646572786462656337343264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.chooseanapptoexclude": {
    resourceKey: "m77696e646f77732e75692e63686f6f7365616e617070746f6578636c756465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.choosearunningapp": {
    resourceKey: "m77696e646f77732e75692e63686f6f73656172756e6e696e67617070",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.choosefiles": {
    resourceKey: "m77696e646f77732e75692e63686f6f736566696c6573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.choosefolder": {
    resourceKey: "m77696e646f77732e75692e63686f6f7365666f6c646572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.choosehowfarbacktostartthenbringthemin": {
    resourceKey: "m77696e646f77732e75692e63686f6f7365686f776661726261636b746f73746172747468656e6272696e677468656d696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.choosethefolderyourscreenshotssaveto": {
    resourceKey: "m77696e646f77732e75692e63686f6f7365746865666f6c646572796f757273637265656e73686f747373617665746f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.choosewhatstaysoutofyourjournal": {
    resourceKey: "m77696e646f77732e75692e63686f6f73657768617473746179736f75746f66796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.claimednotverified": {
    resourceKey: "m77696e646f77732e75692e636c61696d65646e6f747665726966696564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.clickthenpressyourshortcut": {
    resourceKey: "m77696e646f77732e75692e636c69636b7468656e7072657373796f757273686f7274637574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.closenavigation": {
    resourceKey: "m77696e646f77732e75692e636c6f73656e617669676174696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.computersound": {
    resourceKey: "m77696e646f77732e75692e636f6d7075746572736f756e64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.computersoundlevel": {
    resourceKey: "m77696e646f77732e75692e636f6d7075746572736f756e646c6576656c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.confirmthemark": {
    resourceKey: "m77696e646f77732e75692e636f6e6669726d7468656d61726b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.confirmthemarkarg0waiting": {
    resourceKey: "m77696e646f77732e75692e636f6e6669726d7468656d61726b6172673077616974696e67",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.confirmyourjournalsmarktoopen": {
    resourceKey: "m77696e646f77732e75692e636f6e6669726d796f75726a6f75726e616c736d61726b746f6f70656e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.connectajournal": {
    resourceKey: "m77696e646f77732e75692e636f6e6e656374616a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.connected": {
    resourceKey: "m77696e646f77732e75692e636f6e6e6563746564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.connectedto": {
    resourceKey: "m77696e646f77732e75692e636f6e6e6563746564746f",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.connecting": {
    resourceKey: "m77696e646f77732e75692e636f6e6e656374696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.connectingx3d6df245": {
    resourceKey: "m77696e646f77732e75692e636f6e6e656374696e67783364366466323435",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.connection": {
    resourceKey: "m77696e646f77732e75692e636f6e6e656374696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.connecttoyourjournal": {
    resourceKey: "m77696e646f77732e75692e636f6e6e656374746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.connectyourjournal": {
    resourceKey: "m77696e646f77732e75692e636f6e6e656374796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.continuethisconnection": {
    resourceKey: "m77696e646f77732e75692e636f6e74696e756574686973636f6e6e656374696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.copied": {
    resourceKey: "m77696e646f77732e75692e636f70696564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.copythefulllinkfromyourchatandopenitagain": {
    resourceKey: "m77696e646f77732e75692e636f707974686566756c6c6c696e6b66726f6d796f757263686174616e646f70656e6974616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotpausetryagain": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f747061757365747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotresumetryagain": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f74726573756d65747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotsavethischoiceitmay": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f74736176657468697363686f69636569746d6179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotsavethisdismissaltryagain": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f7473617665746869736469736d697373616c747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotsaveturningthisoffitisoffnowbutmayturnon": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f74736176657475726e696e67746869736f6666697469736f66666e6f776275746d61797475726e6f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotsaveturningthisonitstaysofftryagain": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f74736176657475726e696e67746869736f6e697473746179736f6666747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotsaveyoursetupprogresstry": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f7473617665796f7572736574757070726f6772657373747279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldnotupdatestartatlogin": {
    resourceKey: "m77696e646f77732e75692e636f756c646e6f74757064617465737461727461746c6f67696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntcheckforupdates": {
    resourceKey: "m77696e646f77732e75692e636f756c646e74636865636b666f7275706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntcheckforupdatessolstonewas": {
    resourceKey: "m77696e646f77732e75692e636f756c646e74636865636b666f7275706461746573736f6c73746f6e65776173",
    formatIntent: "text",
    arguments: [
      { name: "v", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.couldntcountyourscreenshots": {
    resourceKey: "m77696e646f77732e75692e636f756c646e74636f756e74796f757273637265656e73686f7473",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntdownloadsolstone": {
    resourceKey: "m77696e646f77732e75692e636f756c646e74646f776e6c6f6164736f6c73746f6e65",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.couldntdownloadupdate": {
    resourceKey: "m77696e646f77732e75692e636f756c646e74646f776e6c6f6164757064617465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntfindascreenshotsfolderonthispcchoosethefo": {
    resourceKey: "m77696e646f77732e75692e636f756c646e7466696e646173637265656e73686f7473666f6c6465726f6e74686973706363686f6f7365746865666f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntfinishdiscardingwhatsleft": {
    resourceKey: "m77696e646f77732e75692e636f756c646e7466696e69736864697363617264696e6777686174736c656674",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntinstallupdate": {
    resourceKey: "m77696e646f77732e75692e636f756c646e74696e7374616c6c757064617465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntloadthesolstoneapps": {
    resourceKey: "m77696e646f77732e75692e636f756c646e746c6f6164746865736f6c73746f6e6561707073",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntopenthestorepagein": {
    resourceKey: "m77696e646f77732e75692e636f756c646e746f70656e74686573746f726570616765696e",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.couldntopenyourjournal": {
    resourceKey: "m77696e646f77732e75692e636f756c646e746f70656e796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntopenyourjournaltryagain": {
    resourceKey: "m77696e646f77732e75692e636f756c646e746f70656e796f75726a6f75726e616c747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntreachyourjournaloveryour": {
    resourceKey: "m77696e646f77732e75692e636f756c646e747265616368796f75726a6f75726e616c6f766572796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntreadimportsonthisdevicenothinghasbeenremo": {
    resourceKey: "m77696e646f77732e75692e636f756c646e7472656164696d706f7274736f6e746869736465766963656e6f7468696e676861736265656e72656d6f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntreadyoursavedsourcechoices": {
    resourceKey: "m77696e646f77732e75692e636f756c646e7472656164796f75727361766564736f7572636563686f69636573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntregisterthatshortcut": {
    resourceKey: "m77696e646f77732e75692e636f756c646e7472656769737465727468617473686f7274637574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntremovethistryagain": {
    resourceKey: "m77696e646f77732e75692e636f756c646e7472656d6f766574686973747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntturntryagain": {
    resourceKey: "m77696e646f77732e75692e636f756c646e747475726e747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.couldntverifyjournalsonothing": {
    resourceKey: "m77696e646f77732e75692e636f756c646e747665726966796a6f75726e616c736f6e6f7468696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.dayago": {
    resourceKey: "m77696e646f77732e75692e64617961676f",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.december": {
    resourceKey: "m77696e646f77732e75692e646563656d626572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.delivered": {
    resourceKey: "m77696e646f77732e75692e64656c697665726564",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.deliveredpending": {
    resourceKey: "m77696e646f77732e75692e64656c69766572656470656e64696e67",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "value2", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.desktopactivity": {
    resourceKey: "m77696e646f77732e75692e6465736b746f706163746976697479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.devicechoice": {
    resourceKey: "m77696e646f77732e75692e64657669636563686f696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.devicepriority": {
    resourceKey: "m77696e646f77732e75692e6465766963657072696f72697479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.disable": {
    resourceKey: "m77696e646f77732e75692e64697361626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.dontstartatlogin": {
    resourceKey: "m77696e646f77732e75692e646f6e74737461727461746c6f67696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.downloaddeferredconnectionstatusunknown": {
    resourceKey: "m77696e646f77732e75692e646f776e6c6f61646465666572726564636f6e6e656374696f6e737461747573756e6b6e6f776e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.downloaddeferredmeteredconnection": {
    resourceKey: "m77696e646f77732e75692e646f776e6c6f616464656665727265646d657465726564636f6e6e656374696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.downloaddeferredwhileroaming": {
    resourceKey: "m77696e646f77732e75692e646f776e6c6f616464656665727265647768696c65726f616d696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.downloadingsolstone": {
    resourceKey: "m77696e646f77732e75692e646f776e6c6f6164696e67736f6c73746f6e65",
    formatIntent: "text",
    arguments: [
      { name: "v", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.downloadthelatestfromsolstoneappdownloadwindows": {
    resourceKey: "m77696e646f77732e75692e646f776e6c6f61647468656c617465737466726f6d736f6c73746f6e65617070646f776e6c6f616477696e646f7773",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.edgeandfirefoxsettoenglish": {
    resourceKey: "m77696e646f77732e75692e65646765616e6466697265666f78736574746f656e676c697368",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.enable": {
    resourceKey: "m77696e646f77732e75692e656e61626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.enablethepauseresumeshortcut": {
    resourceKey: "m77696e646f77732e75692e656e61626c657468657061757365726573756d6573686f7274637574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.errorword": {
    resourceKey: "m77696e646f77732e75692e6572726f72776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.esctocancel": {
    resourceKey: "m77696e646f77732e75692e657363746f63616e63656c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.everypicturesavedinarg0comesinnotonlyyourscreens": {
    resourceKey: "m77696e646f77732e75692e6576657279706963747572657361766564696e61726730636f6d6573696e6e6f746f6e6c79796f757273637265656e73",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.everythingisinyourjournal": {
    resourceKey: "m77696e646f77732e75692e65766572797468696e676973696e796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.exclude": {
    resourceKey: "m77696e646f77732e75692e6578636c756465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.exclusionactivity": {
    resourceKey: "m77696e646f77732e75692e6578636c7573696f6e6163746976697479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.failedtoacquiresetuplock": {
    resourceKey: "m77696e646f77732e75692e6661696c6564746f6163717569726573657475706c6f636b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.february": {
    resourceKey: "m77696e646f77732e75692e6665627275617279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.file": {
    resourceKey: "m77696e646f77732e75692e66696c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.files": {
    resourceKey: "m77696e646f77732e75692e66696c6573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.finishconnecting": {
    resourceKey: "m77696e646f77732e75692e66696e697368636f6e6e656374696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.finishconnectingtoyourjournal": {
    resourceKey: "m77696e646f77732e75692e66696e697368636f6e6e656374696e67746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.finishthecurrentconnectionthentryagain": {
    resourceKey: "m77696e646f77732e75692e66696e69736874686563757272656e74636f6e6e656374696f6e7468656e747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.folder": {
    resourceKey: "m77696e646f77732e75692e666f6c646572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.foundjournalpc": {
    resourceKey: "m77696e646f77732e75692e666f756e646a6f75726e616c7063",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.from": {
    resourceKey: "m77696e646f77732e75692e66726f6d",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.frombeforetoday": {
    resourceKey: "m77696e646f77732e75692e66726f6d6265666f7265746f646179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.gettingready": {
    resourceKey: "m77696e646f77732e75692e67657474696e677265616479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.gettingreadyconnectingtoyourjournal": {
    resourceKey: "m77696e646f77732e75692e67657474696e677265616479636f6e6e656374696e67746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.gettingthisdeviceready": {
    resourceKey: "m77696e646f77732e75692e67657474696e67746869736465766963657265616479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.globalshortcut": {
    resourceKey: "m77696e646f77732e75692e676c6f62616c73686f7274637574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.goesalongwithit": {
    resourceKey: "m77696e646f77732e75692e676f6573616c6f6e67776974686974",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.goodafternoon": {
    resourceKey: "m77696e646f77732e75692e676f6f6461667465726e6f6f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.goodevening": {
    resourceKey: "m77696e646f77732e75692e676f6f646576656e696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.goodmorning": {
    resourceKey: "m77696e646f77732e75692e676f6f646d6f726e696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.holdoneormoreofctrlalt": {
    resourceKey: "m77696e646f77732e75692e686f6c646f6e656f726d6f72656f666374726c616c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.home": {
    resourceKey: "m77696e646f77732e75692e686f6d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.homex4e7a5cec": {
    resourceKey: "m77696e646f77732e75692e686f6d65783465376135636563",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.hourago": {
    resourceKey: "m77696e646f77732e75692e686f757261676f",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.howoftentocheckforupdates": {
    resourceKey: "m77696e646f77732e75692e686f776f6674656e746f636865636b666f7275706461746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.hrlefthrminleftboth": {
    resourceKey: "m77696e646f77732e75692e68726c65667468726d696e6c656674626f7468",
    formatIntent: "text",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "count2", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.idle": {
    resourceKey: "m77696e646f77732e75692e69646c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.ifyouhaventyetaddthe": {
    resourceKey: "m77696e646f77732e75692e6966796f75686176656e74796574616464746865",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.import": {
    resourceKey: "m77696e646f77732e75692e696d706f7274",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.importswaitonthisdeviceuntilyouconnectajournal": {
    resourceKey: "m77696e646f77732e75692e696d706f727473776169746f6e74686973646576696365756e74696c796f75636f6e6e656374616a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.importswaituntilyouconfirmyourjournalsmark": {
    resourceKey: "m77696e646f77732e75692e696d706f72747377616974756e74696c796f75636f6e6669726d796f75726a6f75726e616c736d61726b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.importswaituntilyourjournalisupdated": {
    resourceKey: "m77696e646f77732e75692e696d706f72747377616974756e74696c796f75726a6f75726e616c697375706461746564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.inputgain": {
    resourceKey: "m77696e646f77732e75692e696e7075746761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.installing": {
    resourceKey: "m77696e646f77732e75692e696e7374616c6c696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.installingsolstone": {
    resourceKey: "m77696e646f77732e75692e696e7374616c6c696e67736f6c73746f6e65",
    formatIntent: "text",
    arguments: [
      { name: "v", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.inyourjournal": {
    resourceKey: "m77696e646f77732e75692e696e796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.inyourjournalchoosepairadeviceandpastethelinkher": {
    resourceKey: "m77696e646f77732e75692e696e796f75726a6f75726e616c63686f6f73657061697261646576696365616e6470617374657468656c696e6b686572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.itinstallsthenexttimethesolstoneappstartsincludi": {
    resourceKey: "m77696e646f77732e75692e6974696e7374616c6c737468656e65787474696d65746865736f6c73746f6e65617070737461727473696e636c756469",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.itslargerthanyourjournaltakes": {
    resourceKey: "m77696e646f77732e75692e6974736c61726765727468616e796f75726a6f75726e616c74616b6573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.january": {
    resourceKey: "m77696e646f77732e75692e6a616e75617279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.journaldetails": {
    resourceKey: "m77696e646f77732e75692e6a6f75726e616c64657461696c73",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.journallastreached": {
    resourceKey: "m77696e646f77732e75692e6a6f75726e616c6c61737472656163686564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.journalneedsupdatebeforecantake": {
    resourceKey: "m77696e646f77732e75692e6a6f75726e616c6e656564737570646174656265666f726563616e74616b65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.journalsync": {
    resourceKey: "m77696e646f77732e75692e6a6f75726e616c73796e63",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.journalupdateneeded": {
    resourceKey: "m77696e646f77732e75692e6a6f75726e616c7570646174656e6565646564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.journalversion": {
    resourceKey: "m77696e646f77732e75692e6a6f75726e616c76657273696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.july": {
    resourceKey: "m77696e646f77732e75692e6a756c79",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.june": {
    resourceKey: "m77696e646f77732e75692e6a756e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.keeponwhilepausedorscreenis": {
    resourceKey: "m77696e646f77732e75692e6b6565706f6e7768696c657061757365646f7273637265656e6973",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.keepprivatewindowsoutofyourjournal": {
    resourceKey: "m77696e646f77732e75692e6b6565707072697661746577696e646f77736f75746f66796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.keptonthispc": {
    resourceKey: "m77696e646f77732e75692e6b6570746f6e746869737063",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.keptonthispcnotreachingyour": {
    resourceKey: "m77696e646f77732e75692e6b6570746f6e7468697370636e6f747265616368696e67796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.label": {
    resourceKey: "m77696e646f77732e75692e6c6162656c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.lastchecked": {
    resourceKey: "m77696e646f77732e75692e6c617374636865636b6564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.lastreached": {
    resourceKey: "m77696e646f77732e75692e6c61737472656163686564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.lasttakenin": {
    resourceKey: "m77696e646f77732e75692e6c61737474616b656e696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.leftout": {
    resourceKey: "m77696e646f77732e75692e6c6566746f7574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.lessthanaminuteleft": {
    resourceKey: "m77696e646f77732e75692e6c6573737468616e616d696e7574656c656674",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.madebysolpbc": {
    resourceKey: "m77696e646f77732e75692e6d6164656279736f6c706263",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.march": {
    resourceKey: "m77696e646f77732e75692e6d61726368",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.mark": {
    resourceKey: "m77696e646f77732e75692e6d61726b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.markunavailable": {
    resourceKey: "m77696e646f77732e75692e6d61726b756e617661696c61626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.markunavailablex5acbd11e": {
    resourceKey: "m77696e646f77732e75692e6d61726b756e617661696c61626c65783561636264313165",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.may": {
    resourceKey: "m77696e646f77732e75692e6d6179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.microphone": {
    resourceKey: "m77696e646f77732e75692e6d6963726f70686f6e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.microphonelevel": {
    resourceKey: "m77696e646f77732e75692e6d6963726f70686f6e656c6576656c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.microphones": {
    resourceKey: "m77696e646f77732e75692e6d6963726f70686f6e6573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.minuteago": {
    resourceKey: "m77696e646f77732e75692e6d696e75746561676f",
    formatIntent: "cardinal",
    selectorVar: "count",
    arguments: [
      { name: "count", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.moreaboutthis": {
    resourceKey: "m77696e646f77732e75692e6d6f726561626f757474686973",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.moreimports": {
    resourceKey: "m77696e646f77732e75692e6d6f7265696d706f727473",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.movedown": {
    resourceKey: "m77696e646f77732e75692e6d6f7665646f776e",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.moveup": {
    resourceKey: "m77696e646f77732e75692e6d6f76657570",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.n1passwordbitwardenandtheotherpasswordmanager": {
    resourceKey: "m77696e646f77732e75692e6e3170617373776f726462697477617264656e616e647468656f7468657270617373776f72646d616e61676572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.needsattentionword": {
    resourceKey: "m77696e646f77732e75692e6e65656473617474656e74696f6e776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.needspermission": {
    resourceKey: "m77696e646f77732e75692e6e656564737065726d697373696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.next": {
    resourceKey: "m77696e646f77732e75692e6e657874",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nextsave": {
    resourceKey: "m77696e646f77732e75692e6e65787473617665",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nobrowserconnectednow": {
    resourceKey: "m77696e646f77732e75692e6e6f62726f77736572636f6e6e65637465646e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nocredentialaccess": {
    resourceKey: "m77696e646f77732e75692e6e6f63726564656e7469616c616363657373",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.noexclusionsactive": {
    resourceKey: "m77696e646f77732e75692e6e6f6578636c7573696f6e73616374697665",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.noidentitypresented": {
    resourceKey: "m77696e646f77732e75692e6e6f6964656e7469747970726573656e746564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.noinputdevice": {
    resourceKey: "m77696e646f77732e75692e6e6f696e707574646576696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nokeywordsyet": {
    resourceKey: "m77696e646f77732e75692e6e6f6b6579776f726473796574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nolocaljournaltoconnectto": {
    resourceKey: "m77696e646f77732e75692e6e6f6c6f63616c6a6f75726e616c746f636f6e6e656374746f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nomicrophonefound": {
    resourceKey: "m77696e646f77732e75692e6e6f6d6963726f70686f6e65666f756e64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nomicrophoneinputdevices": {
    resourceKey: "m77696e646f77732e75692e6e6f6d6963726f70686f6e65696e70757464657669636573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nomicrophoneisavailableconnectorenableoneinwindo": {
    resourceKey: "m77696e646f77732e75692e6e6f6d6963726f70686f6e656973617661696c61626c65636f6e6e6563746f72656e61626c656f6e65696e77696e646f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nonedisplay": {
    resourceKey: "m77696e646f77732e75692e6e6f6e65646973706c6179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nootherappsrunning": {
    resourceKey: "m77696e646f77732e75692e6e6f6f746865726170707372756e6e696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.noroommorerightnownew": {
    resourceKey: "m77696e646f77732e75692e6e6f726f6f6d6d6f726572696768746e6f776e6577",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.noshortcutset": {
    resourceKey: "m77696e646f77732e75692e6e6f73686f7274637574736574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nosoundoutputfound": {
    resourceKey: "m77696e646f77732e75692e6e6f736f756e646f7574707574666f756e64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nosoundoutputisavailableconnectorenableoneinwind": {
    resourceKey: "m77696e646f77732e75692e6e6f736f756e646f75747075746973617661696c61626c65636f6e6e6563746f72656e61626c656f6e65696e77696e64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.not": {
    resourceKey: "m77696e646f77732e75692e6e6f74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notanswered": {
    resourceKey: "m77696e646f77732e75692e6e6f74616e737765726564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notavailable": {
    resourceKey: "m77696e646f77732e75692e6e6f74617661696c61626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notavailablerightnow": {
    resourceKey: "m77696e646f77732e75692e6e6f74617661696c61626c6572696768746e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notcheckedforupdatesyet": {
    resourceKey: "m77696e646f77732e75692e6e6f74636865636b6564666f7275706461746573796574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notesjournal": {
    resourceKey: "m77696e646f77732e75692e6e6f7465736a6f75726e616c",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "value2", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.notgoingtoyourjournalrightnoweachonesayswhy": {
    resourceKey: "m77696e646f77732e75692e6e6f74676f696e67746f796f75726a6f75726e616c72696768746e6f77656163686f6e6573617973776879",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nothingexcludedyet": {
    resourceKey: "m77696e646f77732e75692e6e6f7468696e676578636c75646564796574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nothingfrombeforetoday": {
    resourceKey: "m77696e646f77732e75692e6e6f7468696e6766726f6d6265666f7265746f646179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nothingwassavedonthisdevice": {
    resourceKey: "m77696e646f77732e75692e6e6f7468696e6777617373617665646f6e74686973646576696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nothingyet": {
    resourceKey: "m77696e646f77732e75692e6e6f7468696e67796574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nothingyetxec5a8da6": {
    resourceKey: "m77696e646f77732e75692e6e6f7468696e67796574786563356138646136",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notonthiscomputer": {
    resourceKey: "m77696e646f77732e75692e6e6f746f6e74686973636f6d7075746572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notreachingyourjournalrightnow": {
    resourceKey: "m77696e646f77732e75692e6e6f747265616368696e67796f75726a6f75726e616c72696768746e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notreported": {
    resourceKey: "m77696e646f77732e75692e6e6f747265706f72746564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.notyet": {
    resourceKey: "m77696e646f77732e75692e6e6f74796574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.nousagedataiseversentupdate": {
    resourceKey: "m77696e646f77732e75692e6e6f75736167656461746169736576657273656e74757064617465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.november": {
    resourceKey: "m77696e646f77732e75692e6e6f76656d626572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.october": {
    resourceKey: "m77696e646f77732e75692e6f63746f626572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.offline": {
    resourceKey: "m77696e646f77732e75692e6f66666c696e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.offlinearg0waiting": {
    resourceKey: "m77696e646f77732e75692e6f66666c696e656172673077616974696e67",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.onlywhatthesolstoneappcantellisascreenshotcomesi": {
    resourceKey: "m77696e646f77732e75692e6f6e6c7977686174746865736f6c73746f6e6561707063616e74656c6c69736173637265656e73686f74636f6d657369",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.onthesitesyouallowthesolstoneextensiontakesinthe": {
    resourceKey: "m77696e646f77732e75692e6f6e7468657369746573796f75616c6c6f77746865736f6c73746f6e65657874656e73696f6e74616b6573696e746865",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.onthisdevice": {
    resourceKey: "m77696e646f77732e75692e6f6e74686973646576696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.openfolder": {
    resourceKey: "m77696e646f77732e75692e6f70656e666f6c646572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.opennavigation": {
    resourceKey: "m77696e646f77732e75692e6f70656e6e617669676174696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.opensettings": {
    resourceKey: "m77696e646f77732e75692e6f70656e73657474696e6773",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.opensoundsettings": {
    resourceKey: "m77696e646f77732e75692e6f70656e736f756e6473657474696e6773",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.openwindowssettings": {
    resourceKey: "m77696e646f77732e75692e6f70656e77696e646f777373657474696e6773",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.otherfilesinarg0staywheretheyare": {
    resourceKey: "m77696e646f77732e75692e6f7468657266696c6573696e6172673073746179776865726574686579617265",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.pageaddresses": {
    resourceKey: "m77696e646f77732e75692e70616765616464726573736573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pageaddressesedgeandchrome": {
    resourceKey: "m77696e646f77732e75692e7061676561646472657373657365646765616e646368726f6d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairagain": {
    resourceKey: "m77696e646f77732e75692e70616972616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairedwith": {
    resourceKey: "m77696e646f77732e75692e70616972656477697468",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.pairing": {
    resourceKey: "m77696e646f77732e75692e70616972696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairingcancellednothingthispchaskept": {
    resourceKey: "m77696e646f77732e75692e70616972696e6763616e63656c6c65646e6f7468696e677468697370636861736b657074",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairingdidntgothroughgeta": {
    resourceKey: "m77696e646f77732e75692e70616972696e676469646e74676f7468726f75676867657461",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairinglinkfromyourjournal": {
    resourceKey: "m77696e646f77732e75692e70616972696e676c696e6b66726f6d796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairthispctoyourjournalto": {
    resourceKey: "m77696e646f77732e75692e70616972746869737063746f796f75726a6f75726e616c746f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairtodelivertoyourjournal": {
    resourceKey: "m77696e646f77732e75692e70616972746f64656c69766572746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pairwithyourjournalfirst": {
    resourceKey: "m77696e646f77732e75692e7061697277697468796f75726a6f75726e616c6669727374",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.partvaultcantberead": {
    resourceKey: "m77696e646f77732e75692e706172747661756c7463616e74626572656164",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.paused": {
    resourceKey: "m77696e646f77732e75692e706175736564",
    formatIntent: "text",
    arguments: [
      { name: "count", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.pausedarg0minleft": {
    resourceKey: "m77696e646f77732e75692e706175736564617267306d696e6c656674",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.pausedlessthanaminuteleft": {
    resourceKey: "m77696e646f77732e75692e7061757365646c6573737468616e616d696e7574656c656674",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pauseduntilyouresume": {
    resourceKey: "m77696e646f77732e75692e706175736564756e74696c796f75726573756d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pausedwiththerestofthesolstone": {
    resourceKey: "m77696e646f77732e75692e70617573656477697468746865726573746f66746865736f6c73746f6e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pausedword": {
    resourceKey: "m77696e646f77732e75692e706175736564776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pausedxa7a9dc5b": {
    resourceKey: "m77696e646f77732e75692e706175736564786137613964633562",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pauseresume": {
    resourceKey: "m77696e646f77732e75692e7061757365726573756d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pausesallsources": {
    resourceKey: "m77696e646f77732e75692e706175736573616c6c736f7572636573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pending": {
    resourceKey: "m77696e646f77732e75692e70656e64696e67",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.placeholder": {
    resourceKey: "m77696e646f77732e75692e706c616365686f6c646572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.placeholder2": {
    resourceKey: "m77696e646f77732e75692e706c616365686f6c64657232",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.presented": {
    resourceKey: "m77696e646f77732e75692e70726573656e746564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.pressyourshortcut": {
    resourceKey: "m77696e646f77732e75692e7072657373796f757273686f7274637574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.previewpaused": {
    resourceKey: "m77696e646f77732e75692e70726576696577706175736564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.privacy": {
    resourceKey: "m77696e646f77732e75692e70726976616379",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.privatewindows": {
    resourceKey: "m77696e646f77732e75692e7072697661746577696e646f7773",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.progresstext": {
    resourceKey: "m77696e646f77732e75692e70726f677265737374657874",
    formatIntent: "text",
    arguments: [
      { name: "stepnum", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
      { name: "totalsteps", type: "cardinal", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.quitsolstone": {
    resourceKey: "m77696e646f77732e75692e71756974736f6c73746f6e65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.readytosetupword": {
    resourceKey: "m77696e646f77732e75692e7265616479746f7365747570776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.recent": {
    resourceKey: "m77696e646f77732e75692e726563656e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.relaunchtoinstall": {
    resourceKey: "m77696e646f77732e75692e72656c61756e6368746f696e7374616c6c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.remove": {
    resourceKey: "m77696e646f77732e75692e72656d6f7665",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.removearg0": {
    resourceKey: "m77696e646f77732e75692e72656d6f766561726730",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.removex7e5608ab": {
    resourceKey: "m77696e646f77732e75692e72656d6f7665783765353630386162",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.restartintake": {
    resourceKey: "m77696e646f77732e75692e72657374617274696e74616b65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.retrying": {
    resourceKey: "m77696e646f77732e75692e7265747279696e67",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.saveacopyofrecentlogs": {
    resourceKey: "m77696e646f77732e75692e7361766561636f70796f66726563656e746c6f6773",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.savedonthisdevicearg0": {
    resourceKey: "m77696e646f77732e75692e73617665646f6e7468697364657669636561726730",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.savingonthisdevicearg0ofarg1": {
    resourceKey: "m77696e646f77732e75692e736176696e676f6e74686973646576696365617267306f6661726731",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg1", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.screen": {
    resourceKey: "m77696e646f77732e75692e73637265656e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.screenalsoaddstheappsyouuse": {
    resourceKey: "m77696e646f77732e75692e73637265656e616c736f6164647374686561707073796f75757365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.screenarg0computersoundarg1micarg2desktopactivit": {
    resourceKey: "m77696e646f77732e75692e73637265656e61726730636f6d7075746572736f756e64617267316d6963617267326465736b746f7061637469766974",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg1", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg2", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "arg3", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.screenpreview": {
    resourceKey: "m77696e646f77732e75692e73637265656e70726576696577",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.screenshots": {
    resourceKey: "m77696e646f77732e75692e73637265656e73686f7473",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.screenshotsalreadyinyourjournalarentbroughtinaga": {
    resourceKey: "m77696e646f77732e75692e73637265656e73686f7473616c7265616479696e796f75726a6f75726e616c6172656e7462726f75676874696e616761",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.screenshotsyoutakewhileitspauseddontcomein": {
    resourceKey: "m77696e646f77732e75692e73637265656e73686f7473796f7574616b657768696c65697473706175736564646f6e74636f6d65696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.segmentsthathavenotreachedyourjournal": {
    resourceKey: "m77696e646f77732e75692e7365676d656e747374686174686176656e6f7472656163686564796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.selecteddevice": {
    resourceKey: "m77696e646f77732e75692e73656c6563746564646576696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.sendingtoyourjournal": {
    resourceKey: "m77696e646f77732e75692e73656e64696e67746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.sendingword": {
    resourceKey: "m77696e646f77732e75692e73656e64696e67776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.september": {
    resourceKey: "m77696e646f77732e75692e73657074656d626572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.set": {
    resourceKey: "m77696e646f77732e75692e736574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.settingupword": {
    resourceKey: "m77696e646f77732e75692e73657474696e677570776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.setupconnectionchanged": {
    resourceKey: "m77696e646f77732e75692e7365747570636f6e6e656374696f6e6368616e676564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.setupisbusytryagainina": {
    resourceKey: "m77696e646f77732e75692e7365747570697362757379747279616761696e696e61",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.setupprogresscouldnotbesaved": {
    resourceKey: "m77696e646f77732e75692e736574757070726f6772657373636f756c646e6f7462657361766564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.setupsequenceisunavailableinthisbuild": {
    resourceKey: "m77696e646f77732e75692e736574757073657175656e63656973756e617661696c61626c65696e746869736275696c64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.setupunavailable": {
    resourceKey: "m77696e646f77732e75692e7365747570756e617661696c61626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.sharesoundfromthiscomputer": {
    resourceKey: "m77696e646f77732e75692e7368617265736f756e6466726f6d74686973636f6d7075746572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.shareyourscreen": {
    resourceKey: "m77696e646f77732e75692e7368617265796f757273637265656e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.shortcut": {
    resourceKey: "m77696e646f77732e75692e73686f7274637574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.shortcutsetbutturnedoff": {
    resourceKey: "m77696e646f77732e75692e73686f72746375747365746275747475726e65646f6666",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.skip": {
    resourceKey: "m77696e646f77732e75692e736b6970",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.sofar": {
    resourceKey: "m77696e646f77732e75692e736f666172",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.solstoneappcantreadvault": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e6561707063616e74726561647661756c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.solstoneappcouldntsaveits": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e65617070636f756c646e7473617665697473",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.solstoneappdidntfindvault": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e656170706469646e7466696e647661756c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.solstoneapptakesnotesonevault": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e6561707074616b65736e6f7465736f6e657661756c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.solstonearg0installsthenexttimethesolstoneappsta": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e6561726730696e7374616c6c737468656e65787474696d65746865736f6c73746f6e65617070737461",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.solstoneisavailable": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e656973617661696c61626c65",
    formatIntent: "text",
    arguments: [
      { name: "v", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.solstoneisreadytoinstall": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e6569737265616479746f696e7374616c6c",
    formatIntent: "text",
    arguments: [
      { name: "v", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.solstonereadyinstall": {
    resourceKey: "m77696e646f77732e75692e736f6c73746f6e657265616479696e7374616c6c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.somenotescouldntgointo": {
    resourceKey: "m77696e646f77732e75692e736f6d656e6f746573636f756c646e74676f696e746f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.somethingansweredpcatisnt": {
    resourceKey: "m77696e646f77732e75692e736f6d657468696e67616e7377657265647063617469736e74",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.somethingansweredpcthroughrelayisn": {
    resourceKey: "m77696e646f77732e75692e736f6d657468696e67616e73776572656470637468726f75676872656c617969736e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.somethingotherthanyourjournalansweredcompare": {
    resourceKey: "m77696e646f77732e75692e736f6d657468696e676f746865727468616e796f75726a6f75726e616c616e737765726564636f6d70617265",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.somethingyouwereaddingdidntfinishsavingadditagai": {
    resourceKey: "m77696e646f77732e75692e736f6d657468696e67796f7577657265616464696e676469646e7466696e697368736176696e67616464697461676169",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.sourcesetupisunfinished": {
    resourceKey: "m77696e646f77732e75692e736f7572636573657475706973756e66696e6973686564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.startfromtoday": {
    resourceKey: "m77696e646f77732e75692e737461727466726f6d746f646179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.state": {
    resourceKey: "m77696e646f77732e75692e7374617465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.status": {
    resourceKey: "m77696e646f77732e75692e737461747573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.storage": {
    resourceKey: "m77696e646f77732e75692e73746f72616765",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.storagelocation": {
    resourceKey: "m77696e646f77732e75692e73746f726167656c6f636174696f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.storeappsallappsfromthemicrosoft": {
    resourceKey: "m77696e646f77732e75692e73746f726561707073616c6c6170707366726f6d7468656d6963726f736f6674",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.storedlocally": {
    resourceKey: "m77696e646f77732e75692e73746f7265646c6f63616c6c79",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.storedonthispc": {
    resourceKey: "m77696e646f77732e75692e73746f7265646f6e746869737063",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.sync": {
    resourceKey: "m77696e646f77732e75692e73796e63",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.text": {
    resourceKey: "m77696e646f77732e75692e74657874",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "value2", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.text3": {
    resourceKey: "m77696e646f77732e75692e7465787433",
    formatIntent: "text",
    arguments: [
      { name: "name", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "name2", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "value3", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "value4", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.text6": {
    resourceKey: "m77696e646f77732e75692e7465787436",
    formatIntent: "text",
    arguments: [
      { name: "status", type: "string", min: null, max: null, numericStyle: "ungrouped" },
      { name: "reason", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.textcontent": {
    resourceKey: "m77696e646f77732e75692e74657874636f6e74656e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.textcontent2": {
    resourceKey: "m77696e646f77732e75692e74657874636f6e74656e7432",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.textcontent4": {
    resourceKey: "m77696e646f77732e75692e74657874636f6e74656e7434",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.textcontent5": {
    resourceKey: "m77696e646f77732e75692e74657874636f6e74656e7435",
    formatIntent: "text",
    arguments: [
      { name: "level", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.textfilestxt": {
    resourceKey: "m77696e646f77732e75692e7465787466696c6573747874",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thatcomboisinusebyanother": {
    resourceKey: "m77696e646f77732e75692e74686174636f6d626f6973696e7573656279616e6f74686572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thatdidnotfinish": {
    resourceKey: "m77696e646f77732e75692e746861746469646e6f7466696e697368",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thatfolderisntobsidianvault": {
    resourceKey: "m77696e646f77732e75692e74686174666f6c64657269736e746f6273696469616e7661756c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thatfoldersnamecant": {
    resourceKey: "m77696e646f77732e75692e74686174666f6c646572736e616d6563616e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thatpairinglinkisntonethe": {
    resourceKey: "m77696e646f77732e75692e7468617470616972696e676c696e6b69736e746f6e65746865",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thearg0folderisntonthispcanymore": {
    resourceKey: "m77696e646f77732e75692e74686561726730666f6c64657269736e746f6e746869737063616e796d6f7265",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.thecopyisalreadygonesothisonlyclearsitfromhere": {
    resourceKey: "m77696e646f77732e75692e746865636f70796973616c7265616479676f6e65736f746869736f6e6c79636c65617273697466726f6d68657265",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thecopyonthisdeviceismissing": {
    resourceKey: "m77696e646f77732e75692e746865636f70796f6e7468697364657669636569736d697373696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thedownloaddidnotfinish": {
    resourceKey: "m77696e646f77732e75692e746865646f776e6c6f61646469646e6f7466696e697368",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thelast30days": {
    resourceKey: "m77696e646f77732e75692e7468656c617374333064617973",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thelast6months": {
    resourceKey: "m77696e646f77732e75692e7468656c617374366d6f6e746873",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thelastcheckforupdatesfailed": {
    resourceKey: "m77696e646f77732e75692e7468656c617374636865636b666f72757064617465736661696c6564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thelastyear": {
    resourceKey: "m77696e646f77732e75692e7468656c61737479656172",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thepairingwindowclosedgetanew": {
    resourceKey: "m77696e646f77732e75692e74686570616972696e6777696e646f77636c6f736564676574616e6577",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thereisntenoughroomonthisdevice": {
    resourceKey: "m77696e646f77732e75692e746865726569736e74656e6f756768726f6f6d6f6e74686973646576696365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.theserulesareactivenowbutcouldn": {
    resourceKey: "m77696e646f77732e75692e746865736572756c65736172656163746976656e6f77627574636f756c646e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thesolstoneappusesonemicrophoneat": {
    resourceKey: "m77696e646f77732e75692e746865736f6c73746f6e65617070757365736f6e656d6963726f70686f6e656174",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thetopenabledmicrophoneisuseduse": {
    resourceKey: "m77696e646f77732e75692e746865746f70656e61626c65646d6963726f70686f6e65697375736564757365",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.theupdatecouldntbeinstalled": {
    resourceKey: "m77696e646f77732e75692e746865757064617465636f756c646e746265696e7374616c6c6564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisbuildcantupdateitself": {
    resourceKey: "m77696e646f77732e75692e746869736275696c6463616e74757064617465697473656c66",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisconnectioncouldnotbesavedtry": {
    resourceKey: "m77696e646f77732e75692e74686973636f6e6e656374696f6e636f756c646e6f7462657361766564747279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisconnectioncouldnotfinishtryagain": {
    resourceKey: "m77696e646f77732e75692e74686973636f6e6e656374696f6e636f756c646e6f7466696e697368747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisconnectioncouldnotfinishtryagain2": {
    resourceKey: "m77696e646f77732e75692e74686973636f6e6e656374696f6e636f756c646e6f7466696e697368747279616761696e32",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thiscouldntbereadadditagain": {
    resourceKey: "m77696e646f77732e75692e74686973636f756c646e746265726561646164646974616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisdevicecantconvertthisphoto": {
    resourceKey: "m77696e646f77732e75692e7468697364657669636563616e74636f6e766572747468697370686f746f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisdevicecouldntopenitssaved": {
    resourceKey: "m77696e646f77732e75692e74686973646576696365636f756c646e746f70656e6974737361766564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisdeviceisnotpairedwithyourjournalimportswaith": {
    resourceKey: "m77696e646f77732e75692e7468697364657669636569736e6f7470616972656477697468796f75726a6f75726e616c696d706f7274737761697468",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisdeviceisntonanetwork": {
    resourceKey: "m77696e646f77732e75692e7468697364657669636569736e746f6e616e6574776f726b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisjournallinkisntvalid": {
    resourceKey: "m77696e646f77732e75692e746869736a6f75726e616c6c696e6b69736e7476616c6964",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thislinkcouldnotbedismissedtry": {
    resourceKey: "m77696e646f77732e75692e746869736c696e6b636f756c646e6f7462656469736d6973736564747279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thislinkcouldnotbeusedtry": {
    resourceKey: "m77696e646f77732e75692e746869736c696e6b636f756c646e6f74626575736564747279",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thislistappliestoactivitytoo": {
    resourceKey: "m77696e646f77732e75692e746869736c6973746170706c696573746f6163746976697479746f6f",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisonlytakesamoment": {
    resourceKey: "m77696e646f77732e75692e746869736f6e6c7974616b6573616d6f6d656e74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thispc": {
    resourceKey: "m77696e646f77732e75692e746869737063",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thispccantconnecttoyour": {
    resourceKey: "m77696e646f77732e75692e74686973706363616e74636f6e6e656374746f796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thispccouldntworkoutyour": {
    resourceKey: "m77696e646f77732e75692e746869737063636f756c646e74776f726b6f7574796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisreadsthewindowtitleandmatches": {
    resourceKey: "m77696e646f77732e75692e74686973726561647374686577696e646f777469746c65616e646d617463686573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisremovesthecopyonthisdevicesoitwontreachyourj": {
    resourceKey: "m77696e646f77732e75692e7468697372656d6f766573746865636f70796f6e74686973646576696365736f6974776f6e747265616368796f75726a",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisremovesthecopyonthisdeviceyourjournalmaystil": {
    resourceKey: "m77696e646f77732e75692e7468697372656d6f766573746865636f70796f6e74686973646576696365796f75726a6f75726e616c6d61797374696c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thissetuplinkcouldnotbeused": {
    resourceKey: "m77696e646f77732e75692e7468697373657475706c696e6b636f756c646e6f74626575736564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisstepcouldnotfinishtryagain": {
    resourceKey: "m77696e646f77732e75692e7468697373746570636f756c646e6f7466696e697368747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.thisstepcouldntfinishtryagain": {
    resourceKey: "m77696e646f77732e75692e7468697373746570636f756c646e7466696e697368747279616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.titlekeywords": {
    resourceKey: "m77696e646f77732e75692e7469746c656b6579776f726473",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.titlekeywordtoexclude": {
    resourceKey: "m77696e646f77732e75692e7469746c656b6579776f7264746f6578636c756465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.toolargebring": {
    resourceKey: "m77696e646f77732e75692e746f6f6c617267656272696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.trynow": {
    resourceKey: "m77696e646f77732e75692e7472796e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.trynowarg0": {
    resourceKey: "m77696e646f77732e75692e7472796e6f7761726730",
    formatIntent: "text",
    arguments: [
      { name: "arg0", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.tryopeningthelinkagain": {
    resourceKey: "m77696e646f77732e75692e7472796f70656e696e677468656c696e6b616761696e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.turningoffstopsbringingchangeswhat": {
    resourceKey: "m77696e646f77732e75692e7475726e696e676f666673746f70736272696e67696e676368616e67657377686174",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.turnitonanytime": {
    resourceKey: "m77696e646f77732e75692e7475726e69746f6e616e7974696d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.turnoff": {
    resourceKey: "m77696e646f77732e75692e7475726e6f6666",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.turnon": {
    resourceKey: "m77696e646f77732e75692e7475726e6f6e",
    formatIntent: "text",
    arguments: [
      { name: "accessiblename", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.unavailable": {
    resourceKey: "m77696e646f77732e75692e756e617661696c61626c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.unavailableword": {
    resourceKey: "m77696e646f77732e75692e756e617661696c61626c65776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.unknown": {
    resourceKey: "m77696e646f77732e75692e756e6b6e6f776e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.unknownjournals": {
    resourceKey: "m77696e646f77732e75692e756e6b6e6f776e6a6f75726e616c73",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.untiliresume": {
    resourceKey: "m77696e646f77732e75692e756e74696c69726573756d65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.usethislink": {
    resourceKey: "m77696e646f77732e75692e757365746869736c696e6b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.usevault": {
    resourceKey: "m77696e646f77732e75692e7573657661756c74",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.value15minutes": {
    resourceKey: "m77696e646f77732e75692e76616c756531356d696e75746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.value1hour": {
    resourceKey: "m77696e646f77732e75692e76616c756531686f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.value30minutes": {
    resourceKey: "m77696e646f77732e75692e76616c756533306d696e75746573",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.vaults": {
    resourceKey: "m77696e646f77732e75692e7661756c7473",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.vaultscomputer": {
    resourceKey: "m77696e646f77732e75692e7661756c7473636f6d7075746572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.vaultwithnamealreadyjournal": {
    resourceKey: "m77696e646f77732e75692e7661756c74776974686e616d65616c72656164796a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.viewdetails": {
    resourceKey: "m77696e646f77732e75692e7669657764657461696c73",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waiting": {
    resourceKey: "m77696e646f77732e75692e77616974696e67",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingforajournalupdateyourscreenshotswaitonthi": {
    resourceKey: "m77696e646f77732e75692e77616974696e67666f72616a6f75726e616c757064617465796f757273637265656e73686f7473776169746f6e746869",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingforyouranswer": {
    resourceKey: "m77696e646f77732e75692e77616974696e67666f72796f7572616e73776572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingforyoutoconfirmyourjournalsmark": {
    resourceKey: "m77696e646f77732e75692e77616974696e67666f72796f75746f636f6e6669726d796f75726a6f75726e616c736d61726b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingjournalupdate": {
    resourceKey: "m77696e646f77732e75692e77616974696e676a6f75726e616c757064617465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingreachjournal": {
    resourceKey: "m77696e646f77732e75692e77616974696e6772656163686a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingsync": {
    resourceKey: "m77696e646f77732e75692e77616974696e6773796e63",
    formatIntent: "text",
    arguments: [
      { name: "value", type: "string", min: null, max: null, numericStyle: "ungrouped" },
    ],
  },
  "windows.ui.waitingtogotoyourjournal": {
    resourceKey: "m77696e646f77732e75692e77616974696e67746f676f746f796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingtogotoyourjournalx5d789db5": {
    resourceKey: "m77696e646f77732e75692e77616974696e67746f676f746f796f75726a6f75726e616c783564373839646235",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingtosync": {
    resourceKey: "m77696e646f77732e75692e77616974696e67746f73796e63",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.waitingword": {
    resourceKey: "m77696e646f77732e75692e77616974696e67776f7264",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.whatanswered": {
    resourceKey: "m77696e646f77732e75692e77686174616e737765726564",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.whatsalreadyinyourjournalstayswhenyoupauseorturn": {
    resourceKey: "m77696e646f77732e75692e7768617473616c7265616479696e796f75726a6f75726e616c73746179737768656e796f7570617573656f727475726e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.whatyousharestaysonthispc": {
    resourceKey: "m77696e646f77732e75692e77686174796f75736861726573746179736f6e746869737063",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.whatyouturnongoesintoyour": {
    resourceKey: "m77696e646f77732e75692e77686174796f757475726e6f6e676f6573696e746f796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.whilethisisonthesolstoneapptakesinsoundandallofi": {
    resourceKey: "m77696e646f77732e75692e7768696c657468697369736f6e746865736f6c73746f6e6561707074616b6573696e736f756e64616e64616c6c6f6669",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.whilethisisonthesolstoneapptakesinthescreenshots": {
    resourceKey: "m77696e646f77732e75692e7768696c657468697369736f6e746865736f6c73746f6e6561707074616b6573696e74686573637265656e73686f7473",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.whilethisisonthesolstoneapptakesinwhatsonyourscr": {
    resourceKey: "m77696e646f77732e75692e7768696c657468697369736f6e746865736f6c73746f6e6561707074616b6573696e77686174736f6e796f7572736372",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.windowsfromtheseappsneverreachyour": {
    resourceKey: "m77696e646f77732e75692e77696e646f777366726f6d7468657365617070736e657665727265616368796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.windowswhosetitlecontainsawordyou": {
    resourceKey: "m77696e646f77732e75692e77696e646f777377686f73657469746c65636f6e7461696e7361776f7264796f75",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.withactivity": {
    resourceKey: "m77696e646f77732e75692e776974686163746976697479",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.withcomputersound": {
    resourceKey: "m77696e646f77732e75692e77697468636f6d7075746572736f756e64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yesterday": {
    resourceKey: "m77696e646f77732e75692e796573746572646179",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.youcanturneachsourceonor": {
    resourceKey: "m77696e646f77732e75692e796f7563616e7475726e65616368736f757263656f6e6f72",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.youchoosewhichsourcesareon": {
    resourceKey: "m77696e646f77732e75692e796f7563686f6f73657768696368736f75726365736172656f6e",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.youllseehowmanyscreenshotstherearebeforeanycomei": {
    resourceKey: "m77696e646f77732e75692e796f756c6c736565686f776d616e7973637265656e73686f747374686572656172656265666f7265616e79636f6d6569",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.your": {
    resourceKey: "m77696e646f77732e75692e796f7572",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournal": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalcanttakethiskindoffile": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c63616e7474616b65746869736b696e646f6666696c65",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalcouldntacceptthis": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c636f756c646e7461636365707474686973",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournaldoesntsupportthismove": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c646f65736e74737570706f7274746869736d6f7665",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalneedsanewlink": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c6e65656473616e65776c696e6b",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalnotsetupyet": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c6e6f747365747570796574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalopenedbutsetupprogresscould": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c6f70656e6564627574736574757070726f6772657373636f756c64",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalshowsthissamemarkbeside": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c73686f77737468697373616d656d61726b626573696465",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalsmarkunavailablerightnow": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c736d61726b756e617661696c61626c6572696768746e6f77",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourjournalstoppedacceptingthisdeviceimportswait": {
    resourceKey: "m77696e646f77732e75692e796f75726a6f75726e616c73746f70706564616363657074696e6774686973646576696365696d706f72747377616974",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yourshortcut": {
    resourceKey: "m77696e646f77732e75692e796f757273686f7274637574",
    formatIntent: "text",
    arguments: [
    ],
  },
  "windows.ui.yousaidthismarkdoesntmatch": {
    resourceKey: "m77696e646f77732e75692e796f7573616964746869736d61726b646f65736e746d61746368",
    formatIntent: "text",
    arguments: [
    ],
  },
};

const BUNDLES = new Map();

function getBundle(locale, useIsolating) {
  const cacheKey = `${locale}:${useIsolating}`;
  let bundle = BUNDLES.get(cacheKey);
  if (!bundle) {
    const locInfo = LOCALES[locale];
    if (!locInfo) throw new Error(`Unknown locale: ${locale}`);
    const ftlSource = CATALOGS[locale];
    if (ftlSource === undefined) throw new Error(`Unknown catalog: ${locale}`);
    const resource = new FluentResource(ftlSource);
    bundle = new FluentBundle(locInfo.bundleLocale, { useIsolating });
    const errors = bundle.addResource(resource);
    if (errors.length > 0) throw new Error(`FTL errors: ${errors.join(", ")}`);
    BUNDLES.set(cacheKey, bundle);
  }
  return bundle;
}

export function render(locale, messageId, args = [], { useIsolating = true } = {}) {
  if (typeof useIsolating !== "boolean") throw new Error("Expected boolean useIsolating");
  const locInfo = LOCALES[locale];
  if (!locInfo) throw new Error(`Unknown locale: ${locale}`);

  const msgInfo = MESSAGES[messageId];
  if (!msgInfo) throw new Error(`Unknown message: ${messageId}`);

  const seen = new Set();
  for (const [k] of args) {
    if (seen.has(k)) throw new Error(`Duplicate argument: ${k}`);
    seen.add(k);
  }

  const declaredMap = new Map(msgInfo.arguments.map(a => [a.name, a]));
  for (const [k] of args) {
    if (!declaredMap.has(k)) throw new Error(`Unknown argument: ${k}`);
  }
  for (const k of declaredMap.keys()) {
    if (!seen.has(k)) throw new Error(`Missing argument: ${k}`);
  }

  const fArgs = {};
  let selectedVariant = msgInfo.enumDefault || null;

  for (const [k, v] of args) {
    const spec = declaredMap.get(k);
    if (spec.type === "string") {
      if (typeof v !== "string") throw new Error(`Expected string for ${k}`);
      if (msgInfo.formatIntent === "enum" && msgInfo.selectorVar === k) {
        if (msgInfo.enumValues.includes(v)) {
          selectedVariant = v;
        } else {
          selectedVariant = msgInfo.enumDefault;
        }
      }
      fArgs[k] = v;
    } else if (spec.type === "boolean") {
      if (typeof v !== "boolean") throw new Error(`Expected boolean for ${k}`);
      fArgs[k] = v ? "true" : "false";
    } else if (spec.type === "integer") {
      if (v && typeof v === "object" && v.type === "cardinal") throw new Error(`Expected integer, got cardinal for ${k}`);
      const num = (v && typeof v === "object" && v.type === "integer") ? v.value : v;
      if (typeof num !== "number" && typeof num !== "bigint") throw new Error(`Expected number for ${k}`);
      if (typeof num === "number" && (!Number.isFinite(num) || !Number.isInteger(num))) throw new Error(`Expected integer number for ${k}`);
      const nVal = BigInt(num);
      const minVal = spec.min !== null ? BigInt(spec.min) : -9007199254740991n;
      const maxVal = spec.max !== null ? BigInt(spec.max) : 9007199254740991n;
      const effMin = minVal < -9007199254740991n ? -9007199254740991n : minVal;
      const effMax = maxVal > 9007199254740991n ? 9007199254740991n : maxVal;
      if (nVal < effMin || nVal > effMax) throw new Error(`Out of range for ${k}`);
      fArgs[k] = Number(nVal);
    } else if (spec.type === "cardinal") {
      if (v && typeof v === "object" && v.type === "integer") throw new Error(`Expected cardinal, got integer for ${k}`);
      const num = (v && typeof v === "object" && v.type === "cardinal") ? v.value : v;
      if (typeof num !== "number" && typeof num !== "bigint") throw new Error(`Expected number for ${k}`);
      if (typeof num === "number" && (!Number.isFinite(num) || !Number.isInteger(num))) throw new Error(`Expected integer number for ${k}`);
      const nVal = BigInt(num);
      const minVal = spec.min !== null ? BigInt(spec.min) : -2147483648n;
      const maxVal = spec.max !== null ? BigInt(spec.max) : 2147483647n;
      const effMin = minVal < -2147483648n ? -2147483648n : minVal;
      const effMax = maxVal > 2147483647n ? 2147483647n : maxVal;
      if (nVal < effMin || nVal > effMax) throw new Error(`Out of range for ${k}`);
      fArgs[k] = Number(nVal);
    }
  }

  if (msgInfo.formatIntent === "cardinal") {
    const select = CATEGORIES[locInfo.cardinal];
    fArgs.i18nCategory = select ? select(Math.abs(fArgs[msgInfo.selectorVar])) : "other";
  }

  const bundle = getBundle(locale, useIsolating);

  const targetKey = msgInfo.formatIntent === "enum" ? (msgInfo.variantKeys[selectedVariant] || msgInfo.defaultKey) : msgInfo.resourceKey;
  const message = bundle.getMessage(targetKey);
  if (!message || !message.value) throw new Error(`Message not found: ${targetKey}`);

  const formatErrors = [];
  const output = bundle.formatPattern(message.value, fArgs, formatErrors);
  if (formatErrors.length > 0) throw new Error(`Format error: ${formatErrors.join(", ")}`);
  return output;
}
