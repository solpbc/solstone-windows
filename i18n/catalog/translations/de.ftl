windows.about.app = Windows-App

windows.about.dayago = { $count ->
        [one] vor { $count } Tag
       *[other] vor { $count } Tagen
    }

windows.about.hourago = { $count ->
        [one] vor { $count } Stunde
       *[other] vor { $count } Stunden
    }

windows.about.journal = Journal

windows.about.justnow = gerade eben

windows.about.lastseen = { $line } · zuletzt gesehen { $age }

windows.about.line0 = { $label } { $version }

windows.about.line1 = { $label } { $version } ({ $build })

windows.about.line2 = { $label } { $version } · { $platform }

windows.about.line3 = { $label } { $version } ({ $build }) · { $platform }

windows.about.line4 = { $label } { $version } · { $arch }

windows.about.line5 = { $label } { $version } ({ $build }) · { $arch }

windows.about.line6 = { $label } { $version } · { $platform } · { $arch }

windows.about.line7 = { $label } { $version } ({ $build }) · { $platform } · { $arch }

windows.about.minuteago = { $count ->
        [one] vor { $count } Minute
       *[other] vor { $count } Minuten
    }

windows.about.unknown = Journal unbekannt

windows.browser.discarded = verworfen

windows.desktop.shared.about.journalunknown = Journal unbekannt

windows.desktop.shared.about.opensourcelocalfirst = quelloffen, lokal zuerst.

windows.desktop.shared.about.pendingsettingstab = Journal

windows.desktop.shared.about.setupwelcomebody1 = die solstone-App nimmt entgegen, was du mit ihr teilst, und alles davon geht in dein Journal.

windows.desktop.shared.about.textcontent = kopieren

windows.desktop.shared.about.textcontent2 = kopieren ist nicht gelungen. markiere den Text und kopiere ihn.

windows.desktop.shared.app.aboutsolstone = über solstone

windows.desktop.shared.app.emptytext = noch nichts

windows.desktop.shared.app.frontendorigin = Einstellungen

windows.desktop.shared.app.justnow = gerade eben

windows.desktop.shared.app.microphonemodeoff = aus

windows.desktop.shared.app.notpaired = nicht gekoppelt

windows.desktop.shared.app.offlinetitle = dein Journal ist nicht erreichbar

windows.desktop.shared.app.settingsdiagnosticson = an

windows.desktop.shared.app.sourcestitle = Quellen

windows.desktop.shared.copy.audiosourcesystem = Systemaudio

windows.desktop.shared.copy.checkagain = erneut prüfen

windows.desktop.shared.copy.choosedevice = ein Gerät wählen

windows.desktop.shared.copy.decidingtitle = deine Auswahl wird gespeichert

windows.desktop.shared.copy.decisionrefusedbody = dein Journal konnte diese Auswahl nicht übernehmen. deine aktuelle Verbindung funktioniert weiter.

windows.desktop.shared.copy.decisionrefusedtitle = Auswahl braucht Aufmerksamkeit

windows.desktop.shared.copy.decisionunknownbody = dein Journal hat das Ergebnis noch nicht bestätigt.

windows.desktop.shared.copy.decisionunknowntitle = deine Auswahl wird geprüft

windows.desktop.shared.copy.deferchoice = nicht jetzt

windows.desktop.shared.copy.journalmarkconfirmbutton = ja, das ist mein Journal

windows.desktop.shared.copy.journalmarkconfirmquestion = stimmt das mit deinem Journal überein?

windows.desktop.shared.copy.journalmarkmismatchbutton = das stimmt nicht überein

windows.desktop.shared.copy.journalmarkunverifiedcancelbutton = Kopplung abbrechen

windows.desktop.shared.copy.journalmarkunverifiedcontinuebutton = trotzdem fortfahren

windows.desktop.shared.copy.journalmarkunverifiedtitle = überprüfen ist nicht gelungen

windows.desktop.shared.copy.keepboth = beide behalten

windows.desktop.shared.copy.listunavailablebody = die Geräte in deinem Journal konnten nicht geladen werden. versuch es noch einmal, wenn dein Journal erreichbar ist.

windows.desktop.shared.copy.listunavailabletitle = Geräte nicht verfügbar

windows.desktop.shared.copy.offlinebody = alles, was aufs Senden wartet, bleibt auf diesem PC. versuch es noch einmal, wenn dein Journal erreichbar ist.

windows.desktop.shared.copy.pickerempty = keine weiteren gekoppelten Geräte

windows.desktop.shared.copy.replaceconfirmbody = dieser PC führt seinen Namen und seinen Verlauf fort. das gewählte Gerät wird den Zugriff auf dein Journal verlieren.

windows.desktop.shared.copy.replaceconfirmtitle = "{ $selecteddevicelabel }" ersetzen?

windows.desktop.shared.copy.replaceconfirmtitlefallback = das gewählte Gerät ersetzen?

windows.desktop.shared.copy.replacedevice = Gerät ersetzen

windows.desktop.shared.copy.replaceofferbody = du kannst beide behalten oder ein Gerät wählen, das du mit diesem ersetzt.

windows.desktop.shared.copy.replaceoffertitle = ersetzt das eines deiner Geräte?

windows.desktop.shared.copy.settingsjournalopen = Journal öffnen

windows.desktop.shared.copy.settingslogexportsave = eine Kopie speichern…

windows.desktop.shared.copy.settingslogexporttitle = aktuelle Protokolle

windows.desktop.shared.copy.settingstabattentiona11y = braucht Aufmerksamkeit

windows.desktop.shared.copy.settingstabupdatesdonea11y = solstone ist auf dem neuesten Stand

windows.desktop.shared.copy.setupapplabel = die solstone-App

windows.desktop.shared.copy.setupback = zurück

windows.desktop.shared.copy.setupconnectedtoyourjournal = verbunden mit deinem Journal

windows.desktop.shared.copy.setupcontinue = fortfahren

windows.desktop.shared.copy.setupjournalcardheading = dein Journal

windows.desktop.shared.copy.setupmicrophoneheadline = deine Gespräche teilen

windows.desktop.shared.copy.setupopenyourjournal = dein Journal öffnen

windows.desktop.shared.copy.setupscreenframecaption = dein Bildschirm, vor einem Moment

windows.desktop.shared.copy.setupwelcomebody2 = dein Journal ist immer privat, nur deins.

windows.desktop.shared.copy.setupwindowtitle = solstone einrichten

windows.desktop.shared.copy.sourcesbrowserconnectednow = jetzt verbunden

windows.desktop.shared.copy.sourcesbrowserdiscard = wartende Browserseiten verwerfen

windows.desktop.shared.copy.sourcesbrowserdiscardcommit = Seiten verwerfen

windows.desktop.shared.copy.sourcesbrowserdiscardconfirm = Browserseiten, die noch aufs Senden warten, werden von diesem PC entfernt.

windows.desktop.shared.copy.sourcesbrowserheld = neue Browserseiten werden gerade nicht entgegengenommen.

windows.desktop.shared.copy.sourcesbrowserpages = Browserseiten

windows.desktop.shared.copy.storageunavailablebody = dieser PC konnte seine gespeicherte Verbindung nicht lesen. alles, was aufs Senden wartet, wurde nicht entfernt.

windows.desktop.shared.copy.storageunavailabletitle = gespeicherte Verbindung nicht verfügbar

windows.desktop.shared.copy.targetmissingbody = das Gerät steht nicht mehr in deinem Journal. wähle ein anderes Gerät, oder behalte beide.

windows.desktop.shared.copy.targetmissingtitle = Gerät nicht mehr verfügbar

windows.desktop.shared.copy.technicaldetails = technische Details

windows.desktop.shared.copy.text = { $value } · { $value2 }

windows.desktop.shared.copy.tryagain = erneut versuchen

windows.desktop.shared.settings.add = hinzufügen

windows.desktop.shared.settings.excludedapps = ausgeschlossene Apps

windows.desktop.shared.settings.gethelp = Hilfe holen

windows.desktop.shared.settings.help = Hilfe

windows.desktop.shared.settings.reportaproblem = ein Problem melden

windows.desktop.shared.settings.severity = ok

windows.desktop.shared.settings.textcontent = erneut versuchen

windows.desktop.shared.settings.updates = Aktualisierungen

windows.desktop.shared.settings.windowtitles = Fenstertitel

windows.desktop.shared.setup.cancel = abbrechen

windows.desktop.shared.setup.pair = koppeln

windows.desktop.shared.setup.startatlogin = beim Anmelden starten

windows.desktop.shared.setup.wizardpausefailure = pausieren

windows.desktop.shared.setup.wizardpausefailure2 = fortsetzen

windows.desktop.shared.updates.automaticupdates = automatische Aktualisierungen

windows.desktop.shared.updates.checkforupdatesautomatically = automatisch nach Aktualisierungen suchen

windows.desktop.shared.updates.checknow = jetzt prüfen

windows.desktop.shared.updates.dismiss = verwerfen

windows.desktop.shared.updates.download = herunterladen

windows.desktop.shared.updates.downloadupdatesinthebackground = Aktualisierungen im Hintergrund herunterladen

windows.desktop.shared.updates.everyday = jeden Tag

windows.desktop.shared.updates.everymonth = jeden Monat

windows.desktop.shared.updates.everyweek = jede Woche

windows.desktop.shared.updates.howoften = wie oft

windows.desktop.shared.updates.nevercheckedforupdates = noch nie nach Aktualisierungen gesucht

windows.desktop.shared.updates.textcontent = die vollständigen Notizen online lesen

windows.desktop.shared.updates.whatsnew = was ist neu

windows.exclusions.activity = { $count ->
        [one] { $display } Bildschirmbild in dieser Sitzung aus deinem Journal herausgehalten
       *[other] { $display } Bildschirmbilder in dieser Sitzung aus deinem Journal herausgehalten
    }

windows.exclusions.activityredacted = { $count ->
        [one] { $display } Bildschirmbild in dieser Sitzung aus deinem Journal herausgehalten · { $more } weitere sind mit einem geschwärzten ausgeschlossenen Fenster in deinem Journal angekommen
       *[other] { $display } Bildschirmbilder in dieser Sitzung aus deinem Journal herausgehalten · { $more } weitere sind mit einem geschwärzten ausgeschlossenen Fenster in deinem Journal angekommen
    }

windows.health.attentioncount = { $count ->
        [one] { $display } braucht Aufmerksamkeit
       *[other] { $display } brauchen Aufmerksamkeit
    }

windows.import.allhistory = der gesamte Importverlauf in deinem Journal

windows.import.expiredword = abgelaufen

windows.import.holdoffline = wartet auf die erneute Verbindung deines Journals.

windows.import.intro = fügt jede Datei hinzu, die du hier wählst oder auf dieses Fenster ablegst. dein Journal findet heraus, was es ist.

windows.import.morehelper = Anleitungen, Optionen und Importverlauf in deinem Journal

windows.import.readytoreviewword = bereit zum Prüfen

windows.import.recenta11y = { $name }, in deinem Journal, { $relative }. öffne in deinem Journal.

windows.import.recentmeta = in deinem Journal · { $relative }

windows.import.removereadybody = das entfernt die Kopie auf diesem Gerät. die wartende Kopie in deinem Journal bleibt dort, bis du sie verwirfst oder sie abläuft.

windows.import.reviewaction = in deinem Journal prüfen

windows.import.reviewactiona11y = { $name } in deinem Journal prüfen

windows.import.reviewdetail = in deinem Journal gespeichert. zeige die Vorschau an und bestätige, um zu starten.

windows.import.reviewdetailbody = in deinem Journal gespeichert. prüfe innerhalb einer Stunde, bevor dein Journal die wartende Kopie entfernt.

windows.import.reviewexpired = die wartende Kopie in deinem Journal ist abgelaufen. dieses Gerät hält die Datei noch.

windows.import.rowsending = wird an dein Journal gesendet…

windows.import.sectionreview = wartet auf deine Prüfung

windows.import.sendagainaction = erneut zum Prüfen senden

windows.import.sendagainactiona11y = { $name } erneut zum Prüfen senden

windows.import.statusattentionheadline = Importe brauchen dich

windows.import.statusattentionsub = öffne den Import, um zu sehen, warum.

windows.import.statusreviewheadline = { $count ->
        [one] { $display } Import bereit zum Prüfen
       *[other] { $display } Importe bereit zum Prüfen
    }

windows.import.statusreviewsub = öffne den Import, um zu prüfen und zu starten.

windows.import.tilea11y = Import, { $state }. { $sub }.

windows.import.tilereadycount = { $display } bereit in deinem Journal

windows.import.tilesendingcount = { $count ->
        [one] { $display } Datei
       *[other] { $display } Dateien
    }

windows.import.tilewaitingcount = { $display } wartet

windows.import.tilewaitingjournal = wartet auf dein Journal

windows.logs.availablelogsareshownbelowcheckthe = verfügbare Protokolle werden unten angezeigt. prüfe die Notizen auf Lücken, bevor du eine Kopie speicherst.

windows.logs.copysaved = Kopie gespeichert.

windows.logs.couldntreadthelogfilestry = die Protokolldateien konnten nicht gelesen werden. versuch es noch einmal.

windows.logs.nologfilesareavailableyettry = es sind noch keine Protokolldateien verfügbar. versuch es noch einmal, nachdem du die App benutzt hast.

windows.logs.norecentlogsareavailableyettry = es sind noch keine aktuellen Protokolle verfügbar. versuch es noch einmal, nachdem du die App benutzt hast.

windows.logs.readthelogsbelowthenchoosewhether = lies die Protokolle unten, und wähle dann, ob du eine Kopie speicherst.

windows.logs.textcontent = lies die aktuellen Protokolle der solstone-App, bevor du eine Kopie für einen Supportbericht speicherst. von hier wird nichts gesendet.

windows.logs.textcontent2 = aktuelle Protokolle lesen

windows.logs.textcontent3 = Protokolle werden gelesen…

windows.logs.textcontent4 = die Protokolle konnten nicht gelesen werden. versuch es noch einmal.

windows.logs.textcontent5 = die Kopie konnte nicht gespeichert werden. versuch, in einem anderen Ordner zu speichern.

windows.notice.everythingsavedonthispcisgoing = alles, was auf diesem PC gespeichert ist, geht jetzt in dein Journal.

windows.notice.everythingyouturnedonisgoinginto = alles, was du eingeschaltet hast, geht wieder in dein Journal.

windows.notice.intakeisrunningagain = Annahme läuft wieder

windows.notice.intakeranintoaproblem = Annahme ist auf ein Problem gestoßen

windows.notice.itsgoingintoyourjournalagain = es geht wieder in dein Journal.

windows.notice.solstoneisstilloneverythingissaved = solstone ist noch an. alles ist auf diesem PC gespeichert und geht in dein Journal, sobald es wieder in Reichweite ist.

windows.notice.theisback = { $value } ist wieder da

windows.notice.theneedsyourpermission = { $value } braucht deine Berechtigung

windows.notice.thesolstoneiconinthetaskbarsays = das solstone-Symbol in der Taskleiste sagt, was nicht stimmt.

windows.notice.windowsprivacysettingsareblockingitturn = Windows-Datenschutzeinstellungen blockieren es. schalte { $value }-Zugriff für Schreibtisch-Apps ein, und es geht wieder in dein Journal.

windows.notice.yourjournalisbackinreach = dein Journal ist wieder in Reichweite

windows.notice.yourjournalisoutofreach = dein Journal ist außer Reichweite

windows.obsidian.chooseobsidianvault = deinen Obsidian-Tresor wählen

windows.obsidian.vault = Tresor

windows.obsidian.warning = dein Journal hat bereits einen Tresor namens { $vault } von einem anderen Gerät. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

windows.obsidian.warningdate = dein Journal hat bereits einen Tresor namens { $vault } von einem anderen Gerät, zuletzt aktualisiert { $date }. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

windows.obsidian.warningdevice = dein Journal hat bereits einen Tresor namens { $vault } von { $device }. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

windows.obsidian.warningdevicedate = dein Journal hat bereits einen Tresor namens { $vault } von { $device }, zuletzt aktualisiert { $date }. wenn du fortfährst, gehen Änderungen von beiden Geräten in dein Journal.

windows.pill.held = die mark bestätigen

windows.pill.heldwaiting = die mark bestätigen · { $waiting } wartet

windows.pill.offline = offline

windows.pill.offlinewaiting = offline · { $waiting } wartet

windows.pill.readytoreview = { $display } bereit zum Prüfen

windows.pill.refused = erneut koppeln

windows.pill.refusedwaiting = erneut koppeln · { $waiting } wartet

windows.pill.syncpaused = Abgleich pausiert

windows.pill.syncpausedwaiting = Abgleich pausiert · { $waiting } wartet

windows.quantity.hours = { $count ->
        [one] { $display } Stunde
       *[other] { $display } Stunden
    }

windows.quantity.items = { $count ->
        [one] { $display } Element
       *[other] { $display } Elemente
    }

windows.quantity.minutes = { $count ->
        [one] { $display } Minute
       *[other] { $display } Minuten
    }

windows.quantity.notes = { $count ->
        [one] { $display } Notiz
       *[other] { $display } Notizen
    }

windows.quantity.pages = { $count ->
        [one] { $display } Seite
       *[other] { $display } Seiten
    }

windows.quantity.screenshots = { $count ->
        [one] { $display } Bildschirmfoto
       *[other] { $display } Bildschirmfotos
    }

windows.screenshots.bringcount = { $count ->
        [one] { $display } Bildschirmfoto hereinholen
       *[other] { $display } Bildschirmfotos hereinholen
    }

windows.screenshots.cantcount = { $count ->
        [one] { $display } Bildschirmfoto kann nicht hereinkommen, also bleibt es im Ordner.
       *[other] { $display } Bildschirmfotos können nicht hereinkommen, also bleiben sie im Ordner.
    }

windows.screenshots.unreadablecount = { $count ->
        [one] { $display } Bildschirmfoto konnte nicht gelesen werden.
       *[other] { $display } Bildschirmfotos konnten nicht gelesen werden.
    }

windows.sharedsources.also = auch auf

windows.sharedsources.labelage = { $label } · { $relative }

windows.sharedsources.labelstate = { $label } · { $state }

windows.sharedsources.labelstateage = { $label } · { $state } · { $relative }

windows.sharedsources.many = von { $count } anderen Geräten

windows.sharedsources.one = von einem anderen Gerät

windows.sharedsources.stateage = { $state } · { $relative }

windows.time.checkedday = { $count ->
        [one] vor { $display } Tag geprüft
       *[other] vor { $display } Tagen geprüft
    }

windows.time.checkedhour = { $count ->
        [one] vor { $display } Stunde geprüft
       *[other] vor { $display } Stunden geprüft
    }

windows.time.checkedminute = { $count ->
        [one] vor { $display } Minute geprüft
       *[other] vor { $display } Minuten geprüft
    }

windows.time.nextsave = in { $minutes }:{ $seconds }

windows.time.takenhours = { $count ->
        [one] vor { $display } Stunde
       *[other] vor { $display } Stunden
    }

windows.tray.checkforupdates = nach Aktualisierungen suchen…

windows.tray.hrmin = { $count } h { $count2 } min

windows.tray.lessthanaminute = weniger als eine Minute

windows.tray.needsarestart = braucht einen Neustart

windows.tray.onbut = an, aber { $needs }

windows.tray.onconnectedtoyourjournal = an, verbunden mit deinem Journal

windows.tray.onnotconnectedtoajournal = an, nicht verbunden mit einem Journal

windows.tray.onsavedonthispc = an, auf diesem PC gespeichert

windows.tray.pausedleft = pausiert, { $value } übrig

windows.tray.solstonewaitingforyoutoconfirmyour = solstone · wartet darauf, dass du die mark deines Journals bestätigst

windows.tray.starting = wird gestartet…

windows.tray.storagefaultforowner = speichern auf diesem PC ist nicht gelungen

windows.tray.text = { $tooltip } { $notice }

windows.tray.theneedspermissioninwindowssettings = { $value } braucht eine Berechtigung in den Windows-Einstellungen

windows.tray.thestoppedgoingintoyourjournal = { $value } hat aufgehört, in dein Journal zu gehen

windows.tray.unknownjournalseenat = unbekanntes Journal bei { $name } gesehen

windows.tray.unknownjournalseenthroughtherelay = unbekanntes Journal über das Relay gesehen

windows.tray.updateavailable = Aktualisierung verfügbar

windows.tray.updatecheckfailed = die Prüfung auf Aktualisierungen ist nicht gelungen

windows.tray.updateready = Aktualisierung bereit · wird beim Beenden und Öffnen installiert

windows.ui.accesswasdenied = Zugriff wurde abgelehnt.

windows.ui.accesswasdeniedcheckwindowspermissionsfor = Zugriff wurde abgelehnt. prüfe die Windows-Berechtigungen für diese Quelle, dann versuch es noch einmal.

windows.ui.activex474d1b75 = aktiv:

windows.ui.activex68f50a9b = { $leading }· aktiv

windows.ui.activex96879611 = aktiv

windows.ui.activity = Bildschirmaktivität

windows.ui.activityisstillon = Bildschirmaktivität ist noch an.

windows.ui.addappnameswindowtitlesandpage = füge deinem Journal App-Namen, Fenstertitel, Seitenadressen und ob du aktiv oder im Leerlauf bist oder dieser PC gesperrt ist hinzu. es ist an, solange der Bildschirm an ist, und pausiert mit ihm. Seitenadressen enthalten die Website und den Pfad, mit nichts nach ? oder #.

windows.ui.addmore = mehr hinzufügen

windows.ui.addspdfsaudioimagesandtextyouchoosehereordropont = wähle Dateien, um sie in dein Journal zu bringen, oder lege sie auf dieses Fenster ab.

windows.ui.addthesolstoneextension = füge die solstone-Erweiterung hinzu.

windows.ui.addto = zu { $value } hinzufügen

windows.ui.aglobalshortcuttopauseandresume = ein globales Tastaturkürzel, um die Annahme von überall zu pausieren und fortzusetzen.

windows.ui.allcaughtup = alles auf dem neuesten Stand

windows.ui.allofit = alles davon

windows.ui.allsourceson = alle Quellen an

windows.ui.alouderinputforquietmicrophoneschanges = ein lauterer Eingang für leise Mikrofone. Änderungen gelten sofort. eine stärkere Verstärkung kann in einem ruhigen Raum mehr Hintergrundgeräusche aufnehmen.

windows.ui.alreadyonhome = schon auf Start

windows.ui.anewlinkfromyourjournalcanbepastedhere = ein neuer Link von deinem Journal kann hier eingefügt werden.

windows.ui.anotherpairinglinkcouldnotbeadded = ein weiterer Kopplungslink konnte nicht hinzugefügt werden.

windows.ui.april = April

windows.ui.arg0 = { $leading }· { $arg0 }

windows.ui.arg0arg1 = { $arg0 }
    { $arg1 }

windows.ui.arg0arg1arg2 = { $arg0 } { $arg1 }, { $arg2 }

windows.ui.arg0bytes = { $arg0 } Byte

windows.ui.arg0gb = { $arg0 } GB

windows.ui.arg0hr = { $arg0 } h

windows.ui.arg0hrleft = { $arg0 } h übrig

windows.ui.arg0kb = { $arg0 } KB

windows.ui.arg0kbx75b11ba1 = { $arg0 } KB

windows.ui.arg0mb = { $arg0 } MB

windows.ui.arg0mbxff882f00 = { $arg0 } MB

windows.ui.arg0min = { $arg0 } min

windows.ui.arg0minago = vor { $arg0 } min

windows.ui.arg0minleft = { $arg0 } min übrig

windows.ui.arg0needsattention = { $arg0 } braucht Aufmerksamkeit

windows.ui.arg0ofarg1inyourjournal = { $arg0 } von { $arg1 } in deinem Journal

windows.ui.arg0ofarg1screenshotsinyourjournal = { $arg0 } von { $arg1 } Bildschirmfotos in deinem Journal

windows.ui.arg0tb = { $arg0 } TB

windows.ui.arg0x211b8ba1 = { $arg0 }%

windows.ui.ascreenshotalreadyinyourjournalstaysthereifyoude = ein Bildschirmfoto, das schon in deinem Journal ist, bleibt dort, wenn du es aus dem Ordner löschst.

windows.ui.ascreenshotthatsonlycopiedtotheclipboardisntsave = ein Bildschirmfoto, das nur in die Zwischenablage kopiert wird, wird nicht gespeichert, also kommt es nicht herein.

windows.ui.asegmentisa5minutelocal = ein Segment ist ein lokales 5-Minuten-Bündel, das hier bleibt, bis dein Journal es empfängt.

windows.ui.attentionneeded = Aufmerksamkeit nötig

windows.ui.audio = Audio

windows.ui.august = August

windows.ui.automaticchecksareoff = automatische Prüfungen sind aus

windows.ui.automaticchecksareon = automatische Prüfungen sind an

windows.ui.awindowthatclosesormovescan = ein Fenster, das sich schließt oder bewegt, kann bis zu einem Bildschirmbild lang erscheinen, bevor der Ausschluss gilt.

windows.ui.backgrounddatauserestricted = Datennutzung im Hintergrund ist eingeschränkt

windows.ui.browsers = Browser

windows.ui.browserstateunavailable = Browserzustand nicht verfügbar

windows.ui.change = ändern

windows.ui.checkedjustnow = gerade eben geprüft

windows.ui.checkforupdates = nach Aktualisierungen suchen

windows.ui.checking = wird geprüft…

windows.ui.checkingforupdates = wird nach Aktualisierungen gesucht…

windows.ui.checkingword = wird geprüft…

windows.ui.checkingyourjournal = dein Journal wird geprüft.

windows.ui.chooseafolder = wähle einen Ordner.

windows.ui.chooseafolderxdbec742d = einen Ordner wählen…

windows.ui.chooseanapptoexclude = eine App zum Ausschließen wählen

windows.ui.choosearunningapp = eine laufende App wählen…

windows.ui.choosefiles = Dateien wählen…

windows.ui.choosefolder = einen Ordner wählen

windows.ui.choosehowfarbacktostartthenbringthemin = wähle, wie weit zurück du anfängst, und bringe sie dann herein.

windows.ui.choosethefolderyourscreenshotssaveto = den Ordner wählen, in dem deine Bildschirmfotos gespeichert werden

windows.ui.choosewhatstaysoutofyourjournal = wähle, was außerhalb deines Journals bleibt. Änderungen gelten sofort.

windows.ui.claimednotverified = behauptet, nicht überprüft

windows.ui.clickthenpressyourshortcut = klicke, dann drücke dein Tastaturkürzel

windows.ui.closenavigation = Navigation schließen

windows.ui.computersound = Computerton

windows.ui.computersoundlevel = Computertonlautstärke

windows.ui.confirmthemark = die mark bestätigen

windows.ui.confirmthemarkarg0waiting = die mark bestätigen · { $arg0 } wartet

windows.ui.confirmyourjournalsmarktoopen = die mark deines Journals bestätigen, um es zu öffnen

windows.ui.connectajournal = ein Journal verbinden

windows.ui.connected = verbunden

windows.ui.connectedto = verbunden mit { $value }

windows.ui.connecting = wird verbunden…

windows.ui.connectingx3d6df245 = wird verbunden

windows.ui.connection = Verbindung

windows.ui.connecttoyourjournal = mit deinem Journal verbinden

windows.ui.connectyourjournal = dein Journal verbinden

windows.ui.continuethisconnection = mit dieser Verbindung fortfahren

windows.ui.copied = kopiert

windows.ui.copythefulllinkfromyourchatandopenitagain = kopiere den vollständigen Link aus deinem Chat und öffne ihn erneut.

windows.ui.couldnotpausetryagain = pausieren ist nicht gelungen. versuch es noch einmal.

windows.ui.couldnotresumetryagain = fortsetzen ist nicht gelungen. versuch es noch einmal.

windows.ui.couldnotsavethischoiceitmay = diese Auswahl speichern ist nicht gelungen. sie kann sich nach einem Neustart ändern. versuch es noch einmal.

windows.ui.couldnotsavethisdismissaltryagain = dieses Verwerfen speichern ist nicht gelungen. versuch es noch einmal.

windows.ui.couldnotsaveturningthisoffitisoffnowbutmayturnon = dieses Ausschalten speichern ist nicht gelungen. es ist jetzt aus, kann sich aber nach einem Neustart wieder einschalten. versuch es noch einmal.

windows.ui.couldnotsaveturningthisonitstaysofftryagain = dieses Einschalten speichern ist nicht gelungen. es bleibt aus. versuch es noch einmal.

windows.ui.couldnotsaveyoursetupprogresstry = deinen Ersteinrichtungsfortschritt speichern ist nicht gelungen. versuch es noch einmal.

windows.ui.couldnotupdatestartatlogin = Start bei der Anmeldung aktualisieren ist nicht gelungen

windows.ui.couldntcheckforupdates = nach Aktualisierungen suchen ist nicht gelungen

windows.ui.couldntcheckforupdatessolstonewas = nach Aktualisierungen suchen ist nicht gelungen. solstone { $v } wurde früher gefunden

windows.ui.couldntcountyourscreenshots = deine Bildschirmfotos zählen ist nicht gelungen.

windows.ui.couldntdownloadsolstone = solstone { $value } herunterladen ist nicht gelungen

windows.ui.couldntdownloadupdate = Aktualisierung herunterladen ist nicht gelungen

windows.ui.couldntfindascreenshotsfolderonthispcchoosethefo = ein Screenshots-Ordner auf diesem PC wurde nicht gefunden. wähle den Ordner, in dem deine Bildschirmfotos gespeichert werden.

windows.ui.couldntfinishdiscardingwhatsleft = wegwerfen ließ sich nicht abschließen. was übrig ist, ist noch auf diesem PC.

windows.ui.couldntinstallupdate = Aktualisierung installieren ist nicht gelungen

windows.ui.couldntloadthesolstoneapps = der Status der solstone-App ließ sich gerade eben nicht laden.

windows.ui.couldntopenthestorepagein = die Store-Seite in { $value } ließ sich nicht öffnen.

windows.ui.couldntopenyourjournal = dein Journal ließ sich nicht öffnen.

windows.ui.couldntopenyourjournaltryagain = dein Journal ließ sich nicht öffnen. versuch es noch einmal.

windows.ui.couldntreachyourjournaloveryour = dein Journal war über dein privates Netzwerk nicht erreichbar. tritt demselben WLAN bei wie dein Journal, dann versuch es noch einmal mit einem neuen Kopplungslink von deinem Journal.

windows.ui.couldntreadimportsonthisdevicenothinghasbeenremo = Importe auf diesem Gerät lesen ist nicht gelungen. nichts wurde entfernt. starte die solstone-App neu, um es noch einmal zu versuchen.

windows.ui.couldntreadyoursavedsourcechoices = deine gespeicherten Quellenauswahlen lesen ist nicht gelungen. Quellen sind aus. wähle deine Quellen erneut in den Einstellungen.

windows.ui.couldntregisterthatshortcut = dieses Tastaturkürzel registrieren ist nicht gelungen

windows.ui.couldntremovethistryagain = dies entfernen ist nicht gelungen. versuch es noch einmal.

windows.ui.couldntturntryagain = dies einschalten ist nicht gelungen. versuch es noch einmal.

windows.ui.couldntverifyjournalsonothing = dein Journal ließ sich nicht überprüfen, also hat sich nichts verbunden. versuch es noch einmal, oder füge einen Kopplungslink ein.

windows.ui.dayago = { $count ->
    [one] vor { $count } Tag
   *[other] vor { $count } Tagen
}

windows.ui.december = Dezember

windows.ui.delivered = { $value } zugestellt

windows.ui.deliveredpending = { $value } zugestellt · { $value2 } ausstehend

windows.ui.desktopactivity = Bildschirmaktivität

windows.ui.devicechoice = Geräteauswahl

windows.ui.devicepriority = Gerätepriorität

windows.ui.disable = deaktivieren

windows.ui.dontstartatlogin = bei der Anmeldung nicht starten

windows.ui.downloaddeferredconnectionstatusunknown = Herunterladen zurückgestellt: Verbindungsstatus unbekannt

windows.ui.downloaddeferredmeteredconnection = Herunterladen auf einer getakteten Verbindung zurückgestellt

windows.ui.downloaddeferredwhileroaming = Herunterladen beim Roaming zurückgestellt

windows.ui.downloadingsolstone = solstone { $v } wird heruntergeladen

windows.ui.downloadthelatestfromsolstoneappdownloadwindows = lade das Neueste von solstone.app/download/windows herunter

windows.ui.edgeandfirefoxsettoenglish = Edge und Firefox auf Englisch gesetzt

windows.ui.enable = aktivieren

windows.ui.enablethepauseresumeshortcut = das Tastaturkürzel für Pausieren / Fortsetzen aktivieren

windows.ui.errorword = Fehler

windows.ui.esctocancel = esc zum Abbrechen

windows.ui.everypicturesavedinarg0comesinnotonlyyourscreens = jedes Bild, das in { $arg0 } gespeichert wird, kommt herein, nicht nur deine Bildschirmfotos.

windows.ui.everythingisinyourjournal = alles ist in deinem Journal

windows.ui.exclude = ausschließen

windows.ui.exclusionactivity = Ausschlussaktivität

windows.ui.failedtoacquiresetuplock = Sperre der Ersteinrichtung ließ sich nicht erlangen

windows.ui.february = Februar

windows.ui.file = Datei

windows.ui.files = Dateien

windows.ui.finishconnecting = Verbinden abschließen

windows.ui.finishconnectingtoyourjournal = Verbindung zu deinem Journal abschließen

windows.ui.finishthecurrentconnectionthentryagain = schließe die aktuelle Verbindung ab, dann versuch es noch einmal.

windows.ui.folder = Ordner

windows.ui.foundjournalpc = dein Journal auf diesem PC gefunden

windows.ui.from = von { $value }

windows.ui.frombeforetoday = von vor heute

windows.ui.gettingready = wird vorbereitet…

windows.ui.gettingreadyconnectingtoyourjournal = wird vorbereitet. Verbindung zu deinem Journal.

windows.ui.gettingthisdeviceready = dieser PC wird vorbereitet

windows.ui.globalshortcut = globales Tastaturkürzel

windows.ui.goesalongwithit = geht damit einher

windows.ui.goodafternoon = guten Nachmittag

windows.ui.goodevening = guten Abend

windows.ui.goodmorning = guten Morgen

windows.ui.holdoneormoreofctrlalt = halte eine oder mehrere von Ctrl, Alt, Shift oder Win gedrückt und drücke eine Taste.

windows.ui.home = Start

windows.ui.homex4e7a5cec = ‹ Start

windows.ui.hourago = { $count ->
    [one] vor { $count } Stunde
   *[other] vor { $count } Stunden
}

windows.ui.howoftentocheckforupdates = wie oft nach Aktualisierungen suchen

windows.ui.hrlefthrminleftboth = { $count } h { $count2 } min übrig

windows.ui.idle = im Leerlauf

windows.ui.ifyouhaventyetaddthe = wenn du es noch nicht getan hast, füge die solstone-Erweiterung einem Browser auf diesem PC hinzu. was du teilst, geht in dein Journal.

windows.ui.import = importieren

windows.ui.importswaitonthisdeviceuntilyouconnectajournal = Importe warten auf diesem Gerät, bis du ein Journal verbindest.

windows.ui.importswaituntilyouconfirmyourjournalsmark = Importe warten, bis du die mark deines Journals bestätigst.

windows.ui.importswaituntilyourjournalisupdated = Importe warten, bis dein Journal aktualisiert ist.

windows.ui.inputgain = Eingangsverstärkung

windows.ui.installing = wird installiert…

windows.ui.installingsolstone = solstone { $v } wird installiert…

windows.ui.inyourjournal = in deinem Journal.

windows.ui.inyourjournalchoosepairadeviceandpastethelinkher = in deinem Journal wähle "ein Gerät koppeln" und füge den Link hier ein.

windows.ui.itinstallsthenexttimethesolstoneappstartsincludi = es installiert sich das nächste Mal, wenn die solstone-App startet, auch wenn du dich bei Windows anmeldest

windows.ui.itslargerthanyourjournaltakes = es ist größer, als dein Journal entgegennimmt

windows.ui.january = Januar

windows.ui.journaldetails = Details des Journals

windows.ui.journallastreached = Journal zuletzt erreicht

windows.ui.journalneedsupdatebeforecantake = dein Journal braucht eine Aktualisierung, bevor es diese Notizen entgegennehmen kann. sie warten auf diesem Computer.

windows.ui.journalsync = Journalabgleich

windows.ui.journalupdateneeded = Journalaktualisierung nötig

windows.ui.journalversion = Journalversion

windows.ui.july = Juli

windows.ui.june = Juni

windows.ui.keeponwhilepausedorscreenis = an lassen, während pausiert oder der Bildschirm aus ist

windows.ui.keepprivatewindowsoutofyourjournal = halte private Fenster aus deinem Journal heraus: Edge und Firefox auf Englisch gesetzt

windows.ui.keptonthispc = auf diesem PC behalten

windows.ui.keptonthispcnotreachingyour = auf diesem PC behalten, erreicht dein Journal gerade nicht

windows.ui.label = nicht gekoppelt. koppeln, um dein Journal abzugleichen

windows.ui.lastchecked = zuletzt geprüft

windows.ui.lastreached = zuletzt erreicht

windows.ui.lasttakenin = zuletzt entgegengenommen

windows.ui.leftout = ausgelassen

windows.ui.lessthanaminuteleft = weniger als eine Minute übrig

windows.ui.madebysolpbc = von sol pbc gemacht.

windows.ui.march = März

windows.ui.mark = mark

windows.ui.markunavailable = mark·nicht verfügbar

windows.ui.markunavailablex5acbd11e = mark · nicht verfügbar

windows.ui.may = Mai

windows.ui.microphone = Mikrofon

windows.ui.microphonelevel = Mikrofonlautstärke

windows.ui.microphones = Mikrofone

windows.ui.minuteago = { $count ->
    [one] vor { $count } Minute
   *[other] vor { $count } Minuten
}

windows.ui.moreaboutthis = mehr dazu

windows.ui.moreimports = weitere Importe

windows.ui.movedown = { $name } nach unten verschieben

windows.ui.moveup = { $name } nach oben verschieben

windows.ui.n1passwordbitwardenandtheotherpasswordmanager = 1Password, Bitwarden und die anderen in den Einstellungen aufgeführten Passwortmanager-Apps sind ausgelassen, ein Passwortmanager in deinem Browser aber nicht. du kannst jederzeit pausieren.

windows.ui.needsattentionword = braucht Aufmerksamkeit

windows.ui.needspermission = braucht Berechtigung

windows.ui.next = weiter

windows.ui.nextsave = nächstes Speichern

windows.ui.nobrowserconnectednow = jetzt kein Browser verbunden

windows.ui.nocredentialaccess = kein Zugriff auf Zugangsdaten

windows.ui.noexclusionsactive = keine Ausschlüsse aktiv

windows.ui.noidentitypresented = keine Identität vorgelegt

windows.ui.noinputdevice = kein Eingabegerät

windows.ui.nokeywordsyet = noch keine Schlüsselwörter

windows.ui.nolocaljournaltoconnectto = kein lokales Journal zum Verbinden

windows.ui.nomicrophonefound = kein Mikrofon gefunden.

windows.ui.nomicrophoneinputdevices = keine Mikrofoneingabegeräte

windows.ui.nomicrophoneisavailableconnectorenableoneinwindo = kein Mikrofon ist verfügbar. verbinde oder aktiviere eins in den Soundeinstellungen von Windows. die solstone-App beginnt von selbst, es zu nutzen.

windows.ui.nonedisplay = nichts

windows.ui.nootherappsrunning = keine anderen Apps laufen

windows.ui.noroommorerightnownew = gerade kein Platz für mehr. neue Seiten werden wieder entgegengenommen, sobald Platz ist.

windows.ui.noshortcutset = kein Tastaturkürzel gesetzt

windows.ui.nosoundoutputfound = keine Tonausgabe gefunden.

windows.ui.nosoundoutputisavailableconnectorenableoneinwind = keine Tonausgabe ist verfügbar. verbinde oder aktiviere eine in den Soundeinstellungen von Windows. die solstone-App beginnt von selbst, sie zu nutzen.

windows.ui.not = nicht

windows.ui.notanswered = nicht beantwortet

windows.ui.notavailable = nicht verfügbar

windows.ui.notavailablerightnow = gerade nicht verfügbar.

windows.ui.notcheckedforupdatesyet = noch nicht nach Aktualisierungen gesucht

windows.ui.notesjournal = { $value } von { $value2 } Notizen in deinem Journal

windows.ui.notgoingtoyourjournalrightnoweachonesayswhy = gehen gerade nicht in dein Journal. jedes sagt, warum.

windows.ui.nothingexcludedyet = noch nichts ausgeschlossen

windows.ui.nothingfrombeforetoday = nichts von vor heute

windows.ui.nothingwassavedonthisdevice = nichts wurde auf diesem Gerät gespeichert.

windows.ui.nothingyet = noch nichts

windows.ui.nothingyetxec5a8da6 = noch nichts.

windows.ui.notonthiscomputer = nicht auf diesem Computer

windows.ui.notreachingyourjournalrightnow = dein Journal ist gerade nicht erreichbar

windows.ui.notreported = nicht gemeldet

windows.ui.notyet = noch nicht

windows.ui.nousagedataiseversentupdate = keine Nutzungsdaten werden je gesendet. Prüfungen auf Aktualisierungen holen nur das Versionsmanifest.

windows.ui.november = November

windows.ui.october = Oktober

windows.ui.offline = offline

windows.ui.offlinearg0waiting = offline · { $arg0 } wartet

windows.ui.onlywhatthesolstoneappcantellisascreenshotcomesi = nur, was die solstone-App als Bildschirmfoto erkennen kann, kommt herein.

windows.ui.onthesitesyouallowthesolstoneextensiontakesinthe = auf den Websites, die du erlaubst, nimmt die solstone-Erweiterung den Text und das grobe Layout der Seite entgegen, und alles davon geht in dein Journal.

windows.ui.onthisdevice = auf diesem Gerät

windows.ui.openfolder = Ordner öffnen

windows.ui.opennavigation = Navigation öffnen

windows.ui.opensettings = Einstellungen öffnen

windows.ui.opensoundsettings = Toneinstellungen öffnen

windows.ui.openwindowssettings = Windows-Einstellungen öffnen

windows.ui.otherfilesinarg0staywheretheyare = andere Dateien in { $arg0 } bleiben, wo sie sind.

windows.ui.pageaddresses = Seitenadressen

windows.ui.pageaddressesedgeandchrome = Seitenadressen (Edge und Chrome)

windows.ui.pairagain = erneut koppeln

windows.ui.pairedwith = gekoppelt mit { $value }

windows.ui.pairing = Kopplung

windows.ui.pairingcancellednothingthispchaskept = Kopplung abgebrochen. füge einen frischen Link aus deinem Journal ein, wenn du bereit bist.

windows.ui.pairingdidntgothroughgeta = die Kopplung ist nicht gelungen. hol einen neuen Kopplungs-Link aus deinem Journal und versuch es noch einmal.

windows.ui.pairinglinkfromyourjournal = Kopplungs-Link aus deinem Journal

windows.ui.pairthispctoyourjournalto = diesen PC mit deinem Journal koppeln, um es zu öffnen

windows.ui.pairtodelivertoyourjournal = koppeln, um an dein Journal zuzustellen

windows.ui.pairwithyourjournalfirst = zuerst mit deinem Journal koppeln

windows.ui.partvaultcantberead = ein Teil dieses Tresors kann nicht gelesen werden. prüfe die Berechtigungen der Ordner darin. der Rest des Tresors geht weiter in dein Journal.

windows.ui.paused = pausiert, { $count }

windows.ui.pausedarg0minleft = pausiert, noch { $arg0 } min

windows.ui.pausedlessthanaminuteleft = pausiert, noch weniger als eine Minute

windows.ui.pauseduntilyouresume = pausiert, bis du fortsetzt

windows.ui.pausedwiththerestofthesolstone = pausiert mit dem Rest der solstone-App. Pausieren hält nicht zurück, was schon entgegengenommen wurde.

windows.ui.pausedword = pausiert

windows.ui.pausedxa7a9dc5b = pausiert

windows.ui.pauseresume = pausieren / fortsetzen

windows.ui.pausesallsources = pausiert alle Quellen.

windows.ui.pending = { $value } ausstehend

windows.ui.placeholder = füge einen Kopplungs-Link aus deinem Journal ein

windows.ui.placeholder2 = ein Schlüsselwort, z. B. Banking

windows.ui.presented = vorgelegt

windows.ui.pressyourshortcut = drücke deinen Kurzbefehl…

windows.ui.previewpaused = Vorschau pausiert

windows.ui.privacy = Datenschutz

windows.ui.privatewindows = private Fenster

windows.ui.progresstext = Schritt { $stepnum } von { $totalsteps }

windows.ui.quitsolstone = solstone beenden

windows.ui.readytosetupword = bereit zum Einrichten

windows.ui.recent = zuletzt

windows.ui.relaunchtoinstall = zum Installieren neu starten

windows.ui.remove = { $value } entfernen

windows.ui.removearg0 = { $arg0 } entfernen?

windows.ui.removex7e5608ab = entfernen

windows.ui.restartintake = Annahme neu starten

windows.ui.retrying = { $value } wird erneut versucht

windows.ui.saveacopyofrecentlogs = eine Kopie der letzten Protokolle speichern

windows.ui.savedonthisdevicearg0 = auf diesem Gerät gespeichert: { $arg0 }

windows.ui.savingonthisdevicearg0ofarg1 = wird auf diesem Gerät gespeichert: { $arg0 } von { $arg1 }

windows.ui.screen = Bildschirm

windows.ui.screenalsoaddstheappsyouuse = der Bildschirm fügt außerdem die Apps, die du nutzt, mit Fenstertiteln und Seitenadressen deinem Journal hinzu. das kannst du in den Einstellungen ausschalten.

windows.ui.screenarg0computersoundarg1micarg2desktopactivit = Bildschirm: { $arg0 } · Computer-Ton: { $arg1 } · Mikrofon: { $arg2 } · Bildschirmaktivität: { $arg3 }

windows.ui.screenpreview = Bildschirmvorschau

windows.ui.screenshots = Bildschirmfotos

windows.ui.screenshotsalreadyinyourjournalarentbroughtinaga = Bildschirmfotos, die schon in deinem Journal sind, werden nicht noch einmal hereingeholt.

windows.ui.screenshotsyoutakewhileitspauseddontcomein = Bildschirmfotos, die du machst, während es pausiert ist, kommen nicht herein.

windows.ui.segmentsthathavenotreachedyourjournal = Segmente, die dein Journal noch nicht erreicht haben, werden nie gelöscht.

windows.ui.selecteddevice = ausgewähltes Gerät

windows.ui.sendingtoyourjournal = wird an dein Journal gesendet.

windows.ui.sendingword = wird gesendet

windows.ui.september = September

windows.ui.set = festlegen

windows.ui.settingupword = wird eingerichtet

windows.ui.setupconnectionchanged = die Verbindung der Ersteinrichtung hat sich geändert

windows.ui.setupisbusytryagainina = Ersteinrichtung ist beschäftigt. versuch es in einem Moment noch einmal.

windows.ui.setupprogresscouldnotbesaved = der Fortschritt der Ersteinrichtung konnte nicht gespeichert werden

windows.ui.setupsequenceisunavailableinthisbuild = die Sequenz der Ersteinrichtung ist in diesem Build nicht verfügbar

windows.ui.setupunavailable = Ersteinrichtung nicht verfügbar

windows.ui.sharesoundfromthiscomputer = Ton von diesem PC teilen

windows.ui.shareyourscreen = deinen Bildschirm teilen

windows.ui.shortcut = Kurzbefehl

windows.ui.shortcutsetbutturnedoff = Kurzbefehl festgelegt, aber ausgeschaltet

windows.ui.skip = überspringen

windows.ui.sofar = bisher

windows.ui.solstoneappcantreadvault = die solstone-App kann diesen Tresor nicht lesen. prüfe, ob sein Ordner da ist und ob das Laufwerk, auf dem er liegt, verbunden ist. in deinem Journal ändert sich nichts, solange er außer Reichweite ist.

windows.ui.solstoneappcouldntsaveits = die solstone-App konnte ihren Stand für diesen Tresor nicht speichern. prüfe, ob dieser Computer freien Speicherplatz hat.

windows.ui.solstoneappdidntfindvault = die solstone-App hat auf diesem Computer keinen Tresor gefunden. wähl stattdessen seinen Ordner.

windows.ui.solstoneapptakesnotesonevault = die solstone-App nimmt die Notizen in einem Tresor auf diesem Computer entgegen. alle gehen in dein Journal, danach jede, sobald sie sich ändert. dein Tresor bleibt, wie er ist.

windows.ui.solstonearg0installsthenexttimethesolstoneappsta = solstone { $arg0 } wird installiert, wenn die solstone-App das nächste Mal startet, auch dann, wenn du dich bei Windows anmeldest

windows.ui.solstoneisavailable = solstone { $v } ist verfügbar

windows.ui.solstoneisreadytoinstall = solstone { $v } ist bereit zum Installieren

windows.ui.solstonereadyinstall = solstone ist bereit zum Installieren

windows.ui.somenotescouldntgointo = einige Notizen konnten nicht in dein Journal gehen. die solstone-App versucht sie erneut, wenn sie das nächste Mal startet.

windows.ui.somethingansweredpcatisnt = etwas hat diesem PC an { $value } geantwortet, und es ist nicht das Journal, mit dem dieser PC gekoppelt ist. das kann ein anderes Gerät sein, das diese Adresse jetzt nutzt, oder dein Journal, falls seine Identität zurückgesetzt wurde. wenn die mark unter "was geantwortet hat" mit der übereinstimmt, die dein Journal jetzt zeigt, kopple diesen PC erneut.

windows.ui.somethingansweredpcthroughrelayisn = etwas hat diesem PC über das Relay geantwortet, und es ist nicht das Journal, mit dem dieser PC gekoppelt ist. das ist weniger gewiss als eine direkte Verbindung.

windows.ui.somethingotherthanyourjournalansweredcompare = etwas hat statt deines Journals geantwortet

windows.ui.somethingyouwereaddingdidntfinishsavingadditagai = etwas, das du hinzugefügt hast, wurde nicht fertig gespeichert. füge es erneut hinzu.

windows.ui.sourcesetupisunfinished = die Ersteinrichtung der Quelle ist nicht fertig

windows.ui.startfromtoday = ab heute starten

windows.ui.state = Zustand

windows.ui.status = Status

windows.ui.storage = Speicher

windows.ui.storagelocation = Speicherort

windows.ui.storeappsallappsfromthemicrosoft = Store-Apps (alle). Apps aus dem Microsoft Store teilen sich einen Eintrag, also schließt das Ausschließen dieses Eintrags alle aus.

windows.ui.storedlocally = { $value } lokal gespeichert

windows.ui.storedonthispc = auf diesem PC gespeichert

windows.ui.sync = Abgleich

windows.ui.text = { $name }. { $value2 }

windows.ui.text3 = { $name }, { $name2 } · { $value3 } { $value4 }

windows.ui.text6 = { $status }: { $reason }

windows.ui.textcontent = ein weiterer Kopplungs-Link ist angekommen: verwende ihn oder verwirf ihn.

windows.ui.textcontent2 = wird gekoppelt…

windows.ui.textcontent4 = Kurzbefehl leeren

windows.ui.textcontent5 = { $level }×

windows.ui.textfilestxt = Textdateien (*.txt)

windows.ui.thatcomboisinusebyanother = dieses Tastaturkürzel wird von einer anderen App genutzt. wähl ein anderes.

windows.ui.thatdidnotfinish = das wurde nicht fertig

windows.ui.thatfolderisntobsidianvault = dieser Ordner ist kein Obsidian-Tresor, also wurde nichts gesendet.

windows.ui.thatfoldersnamecant = der Name dieses Ordners kann in deinem Journal nicht der Name eines Tresors sein. benenne den Ordner um und versuch es noch einmal.

windows.ui.thatpairinglinkisntonethe = dieser Kopplungs-Link ist keiner, den die solstone-App lesen kann. hol einen neuen Kopplungs-Link aus deinem Journal und versuch es noch einmal.

windows.ui.thearg0folderisntonthispcanymore = der Ordner { $arg0 } ist nicht mehr auf diesem PC.

windows.ui.thecopyisalreadygonesothisonlyclearsitfromhere = die Kopie ist schon weg, das entfernt sie also nur noch von hier.

windows.ui.thecopyonthisdeviceismissing = die Kopie auf diesem Gerät fehlt

windows.ui.thedownloaddidnotfinish = { $leading }· der Download wurde nicht fertig

windows.ui.thelast30days = die letzten 30 Tage

windows.ui.thelast6months = die letzten 6 Monate

windows.ui.thelastcheckforupdatesfailed = { $leading }· die letzte Prüfung auf Aktualisierungen ist nicht gelungen

windows.ui.thelastyear = das letzte Jahr

windows.ui.thepairingwindowclosedgetanew = das Kopplungsfenster wurde geschlossen. hol einen neuen Kopplungs-Link aus deinem Journal und versuch es dann noch einmal.

windows.ui.thereisntenoughroomonthisdevice = es ist nicht genug Platz auf diesem Gerät

windows.ui.theserulesareactivenowbutcouldn = diese Regeln sind jetzt aktiv, konnten aber nicht gespeichert werden und überstehen einen Neustart daher vielleicht nicht.

windows.ui.thesolstoneappusesonemicrophoneat = die solstone-App nutzt jeweils nur ein Mikrofon. leg fest, welches, und wie stark du es verstärkst.

windows.ui.thetopenabledmicrophoneisuseduse = das oberste aktivierte Mikrofon wird genutzt. nutze ↑ ↓, um die Reihenfolge festzulegen; die solstone-App fällt auf das nächste zurück, wenn eines nicht verfügbar ist.

windows.ui.theupdatecouldntbeinstalled = { $leading }· die Aktualisierung konnte nicht installiert werden

windows.ui.thisbuildcantupdateitself = dieser Build kann sich nicht selbst aktualisieren

windows.ui.thisconnectioncouldnotbesavedtry = diese Verbindung konnte nicht gespeichert werden. versuch es noch einmal, oder füge einen Kopplungs-Link aus deinem Journal ein.

windows.ui.thisconnectioncouldnotfinishtryagain = diese Verbindung konnte nicht fertig werden. versuch es noch einmal, oder füge einen neuen Kopplungs-Link aus deinem Journal ein.

windows.ui.thisconnectioncouldnotfinishtryagain2 = diese Verbindung konnte nicht fertig werden. versuch es noch einmal.

windows.ui.thiscouldntbereadadditagain = das konnte nicht gelesen werden. füge es erneut hinzu

windows.ui.thisdevicecantconvertthisphoto = dieses Gerät kann dieses Foto nicht umwandeln

windows.ui.thisdevicecouldntopenitssaved = dieser PC konnte seine gespeicherte Verbindung zu deinem Journal nicht öffnen. alles, was auf das Senden wartet, ist noch hier. kopple erneut, um sie wieder zu verbinden.

windows.ui.thisdeviceisnotpairedwithyourjournalimportswaith = dieses Gerät ist nicht mit deinem Journal gekoppelt. Importe warten hier, bis du dich mit einem Journal verbindest.

windows.ui.thisdeviceisntonanetwork = dieser PC ist nicht in einem Netzwerk. die Kopplung muss dein Journal direkt erreichen, also tritt demselben WLAN bei wie dein Journal und versuch es noch einmal. alles, was die solstone-App entgegengenommen hat, ist auf diesem PC und wird abgeglichen, sobald du wieder verbunden bist.

windows.ui.thisjournallinkisntvalid = dieser Journal-Link ist nicht gültig.

windows.ui.thislinkcouldnotbedismissedtry = dieser Link konnte nicht verworfen werden. versuch es noch einmal.

windows.ui.thislinkcouldnotbeusedtry = dieser Link konnte nicht verwendet werden. versuch es noch einmal.

windows.ui.thislistappliestoactivitytoo = diese Liste gilt auch für die Bildschirmaktivität

windows.ui.thisonlytakesamoment = das dauert nur einen Moment

windows.ui.thispc = dieser PC

windows.ui.thispccantconnecttoyour = dieser PC kann mit der gespeicherten Kopplung keine Verbindung zu deinem Journal herstellen. hol einen neuen Kopplungs-Link aus deinem Journal und kopple erneut.

windows.ui.thispccouldntworkoutyour = dieser PC konnte die mark deines Journals nicht ermitteln, also gibt es nichts zu vergleichen. fahr nur fort, wenn du sicher bist, dass der Link aus deinem Journal kommt.

windows.ui.thisreadsthewindowtitleandmatches = das liest den Fenstertitel und erkennt private Fenster in Edge und Firefox. das wurde bei Edge 153 und Firefox 156 geprüft, jeweils auf Englisch eingestellt; eine Browser-Aktualisierung oder eine andere Sprache kann einen Titel ändern, und dann kann ein privates Fenster dort dein Journal erreichen. Chrome 153 und Brave 153 zeigen den privaten Modus nicht in ihren Titeln, und andere Browser werden nicht erkannt, also erreichen ihre privaten Fenster dein Journal. um jedes Fenster eines Browsers aus deinem Journal herauszuhalten, füge diesen Browser unten zu den ausgeschlossenen Apps hinzu.

windows.ui.thisremovesthecopyonthisdevicesoitwontreachyourj = das entfernt die Kopie auf diesem Gerät, also erreicht sie dein Journal nicht.

windows.ui.thisremovesthecopyonthisdeviceyourjournalmaystil = das entfernt die Kopie auf diesem Gerät. dein Journal kann die Kopie, die es schon hat, noch hereinholen.

windows.ui.thissetuplinkcouldnotbeused = dieser Link zur Ersteinrichtung konnte nicht verwendet werden. kehr für einen neuen Link zu deinem Journal zurück, oder füge hier einen Kopplungs-Link ein.

windows.ui.thisstepcouldnotfinishtryagain = dieser Schritt konnte nicht fertig werden. versuch es noch einmal.

windows.ui.thisstepcouldntfinishtryagain = dieser Schritt konnte nicht fertig werden. versuch es noch einmal.

windows.ui.titlekeywords = Titel-Schlüsselwörter

windows.ui.titlekeywordtoexclude = Titel-Schlüsselwort zum Ausschließen

windows.ui.toolargebring = zu groß zum Hereinholen

windows.ui.trynow = jetzt versuchen

windows.ui.trynowarg0 = jetzt versuchen, { $arg0 }

windows.ui.tryopeningthelinkagain = versuch, den Link erneut zu öffnen.

windows.ui.turningoffstopsbringingchangeswhat = das Ausschalten stoppt das Hereinholen von Änderungen. was schon in deinem Journal ist, bleibt dort, und was wartet, bleibt auf diesem Computer, bis du es wieder einschaltest.

windows.ui.turnitonanytime = schalte es jederzeit ein.

windows.ui.turnoff = ausschalten

windows.ui.turnon = { $accessiblename } einschalten

windows.ui.unavailable = nicht verfügbar

windows.ui.unavailableword = nicht verfügbar

windows.ui.unknown = unbekannt

windows.ui.unknownjournals = unbekannte Journale

windows.ui.untiliresume = bis ich fortsetze

windows.ui.usethislink = diesen Link verwenden

windows.ui.usevault = diesen Tresor verwenden

windows.ui.value15minutes = 15 Minuten

windows.ui.value1hour = 1 Stunde

windows.ui.value30minutes = 30 Minuten

windows.ui.vaults = Tresore

windows.ui.vaultscomputer = Tresore auf diesem Computer

windows.ui.vaultwithnamealreadyjournal = ein Tresor mit diesem Namen ist schon in deinem Journal

windows.ui.viewdetails = Details anzeigen

windows.ui.waiting = wartet

windows.ui.waitingforajournalupdateyourscreenshotswaitonthi = wartet auf eine Aktualisierung des Journals. deine Bildschirmfotos warten auf diesem PC.

windows.ui.waitingforyouranswer = wartet auf deine Antwort

windows.ui.waitingforyoutoconfirmyourjournalsmark = wartet darauf, dass du die mark deines Journals bestätigst

windows.ui.waitingjournalupdate = wartet auf eine Aktualisierung des Journals

windows.ui.waitingreachjournal = wartet darauf, dein Journal zu erreichen

windows.ui.waitingsync = { $value } wartet auf den Abgleich

windows.ui.waitingtogotoyourjournal = wartet darauf, in dein Journal zu gehen

windows.ui.waitingtogotoyourjournalx5d789db5 = wartet darauf, in dein Journal zu gehen.

windows.ui.waitingtosync = wartet auf den Abgleich

windows.ui.waitingword = wartet

windows.ui.whatanswered = was geantwortet hat

windows.ui.whatsalreadyinyourjournalstayswhenyoupauseorturn = was schon in deinem Journal ist, bleibt, wenn du pausierst oder das ausschaltest.

windows.ui.whatyousharestaysonthispc = was du teilst, bleibt auf diesem PC, bis es in dein Journal auf deinen eigenen Geräten geht. dein Journal ist immer privat, nur deins.

windows.ui.whatyouturnongoesintoyour = was du einschaltest, geht in dein Journal.

windows.ui.whilethisisonthesolstoneapptakesinsoundandallofi = solange das an ist, nimmt die solstone-App Ton entgegen, und alles davon geht in dein Journal.

windows.ui.whilethisisonthesolstoneapptakesinthescreenshots = solange das an ist, nimmt die solstone-App die Bildschirmfotos entgegen, die du auf diesem PC machst, und dein Journal behält jedes in der Minute, in der du es gemacht hast.

windows.ui.whilethisisonthesolstoneapptakesinwhatsonyourscr = solange das an ist, nimmt die solstone-App entgegen, was auf deinem Bildschirm ist, und alles davon geht in dein Journal.

windows.ui.windowsfromtheseappsneverreachyour = Fenster dieser Apps erreichen dein Journal nie.

windows.ui.windowswhosetitlecontainsawordyou = Fenster, deren Titel ein Wort enthält, das du wählst, erreichen dein Journal nie.

windows.ui.withactivity = mit Bildschirmaktivität

windows.ui.withcomputersound = mit Computer-Ton

windows.ui.yesterday = gestern

windows.ui.youcanturneachsourceonor = du kannst jede Quelle in den Quellen ein- oder ausschalten.

windows.ui.youchoosewhichsourcesareon = du wählst, welche Quellen an sind.

windows.ui.youllseehowmanyscreenshotstherearebeforeanycomei = du wirst sehen, wie viele Bildschirmfotos es gibt, bevor eines hereinkommt.

windows.ui.your = dein

windows.ui.yourjournal = dein · Journal

windows.ui.yourjournalcanttakethiskindoffile = dein Journal kann diese Art von Datei nicht entgegennehmen

windows.ui.yourjournalcouldntacceptthis = dein Journal konnte das nicht annehmen.

windows.ui.yourjournaldoesntsupportthismove = dein Journal unterstützt diesen Vorgang noch nicht. alles, was auf das Senden wartet, bleibt auf diesem PC. aktualisiere dein Journal und versuch es dann noch einmal.

windows.ui.yourjournalneedsanewlink = dein Journal braucht einen neuen Link

windows.ui.yourjournalnotsetupyet = dein Journal, noch nicht eingerichtet

windows.ui.yourjournalopenedbutsetupprogresscould = dein Journal wurde geöffnet, aber der Fortschritt der Ersteinrichtung konnte nicht gespeichert werden. versuch es noch einmal.

windows.ui.yourjournalshowsthissamemarkbeside = dein Journal zeigt dieselbe mark neben dem Kopplungs-Link. sie sollte genau übereinstimmen.

windows.ui.yourjournalsmarkunavailablerightnow = die mark deines Journals, gerade nicht verfügbar

windows.ui.yourjournalstoppedacceptingthisdeviceimportswait = dein Journal hat aufgehört, dieses Gerät anzunehmen. Importe warten hier, bis du erneut koppelst.

windows.ui.yourshortcut = dein Kurzbefehl

windows.ui.yousaidthismarkdoesntmatch = du hast gesagt, dass diese mark nicht mit der übereinstimmt, die dein Journal zeigt, also ist dieser PC nicht gekoppelt. du hast vielleicht den falschen Link eingefügt, oder etwas stimmt nicht. füge einen frischen Link aus deinem Journal ein, oder schick eine E-Mail an support@solstone.app, und wir helfen.
