windows.about.app = app di windows

windows.about.dayago = { $count ->
        [one] { $count } giorno fa
        [many] { $count } di giorni fa
       *[other] { $count } giorni fa
    }

windows.about.hourago = { $count ->
        [one] { $count } ora fa
        [many] { $count } di ore fa
       *[other] { $count } ore fa
    }

windows.about.journal = diario

windows.about.justnow = proprio ora

windows.about.lastseen = { $line } · visto l'ultima volta { $age }

windows.about.line0 = { $label } { $version }

windows.about.line1 = { $label } { $version } ({ $build })

windows.about.line2 = { $label } { $version } · { $platform }

windows.about.line3 = { $label } { $version } ({ $build }) · { $platform }

windows.about.line4 = { $label } { $version } · { $arch }

windows.about.line5 = { $label } { $version } ({ $build }) · { $arch }

windows.about.line6 = { $label } { $version } · { $platform } · { $arch }

windows.about.line7 = { $label } { $version } ({ $build }) · { $platform } · { $arch }

windows.about.minuteago = { $count ->
        [one] { $count } minuto fa
        [many] { $count } di minuti fa
       *[other] { $count } minuti fa
    }

windows.about.unknown = diario sconosciuto

windows.browser.discarded = scartato

windows.desktop.shared.about.journalunknown = diario sconosciuto

windows.desktop.shared.about.opensourcelocalfirst = open source, local-first.

windows.desktop.shared.about.pendingsettingstab = diario

windows.desktop.shared.about.setupwelcomebody1 = l'app di solstone prende ciò che condividi con lei, e tutto va nel tuo diario.

windows.desktop.shared.about.textcontent = copia

windows.desktop.shared.about.textcontent2 = non è stato possibile copiare. seleziona il testo e copialo.

windows.desktop.shared.app.aboutsolstone = informazioni su solstone

windows.desktop.shared.app.emptytext = ancora niente

windows.desktop.shared.app.frontendorigin = impostazioni

windows.desktop.shared.app.justnow = proprio ora

windows.desktop.shared.app.microphonemodeoff = disattivo

windows.desktop.shared.app.notpaired = non accoppiato

windows.desktop.shared.app.offlinetitle = non è stato possibile connettersi al tuo diario

windows.desktop.shared.app.settingsdiagnosticson = attivo

windows.desktop.shared.app.sourcestitle = fonti

windows.desktop.shared.copy.audiosourcesystem = audio di sistema

windows.desktop.shared.copy.checkagain = controlla di nuovo

windows.desktop.shared.copy.choosedevice = scegli un dispositivo

windows.desktop.shared.copy.decidingtitle = salvataggio della tua scelta

windows.desktop.shared.copy.decisionrefusedbody = il tuo diario non ha potuto applicare questa scelta. la connessione attuale funziona ancora.

windows.desktop.shared.copy.decisionrefusedtitle = la scelta richiede attenzione

windows.desktop.shared.copy.decisionunknownbody = il tuo diario non ha ancora confermato il risultato.

windows.desktop.shared.copy.decisionunknowntitle = controllo della tua scelta

windows.desktop.shared.copy.deferchoice = non ora

windows.desktop.shared.copy.journalmarkconfirmbutton = sì, questo è il mio diario

windows.desktop.shared.copy.journalmarkconfirmquestion = corrisponde al tuo diario?

windows.desktop.shared.copy.journalmarkmismatchbutton = non corrisponde

windows.desktop.shared.copy.journalmarkunverifiedcancelbutton = annulla l'accoppiamento

windows.desktop.shared.copy.journalmarkunverifiedcontinuebutton = continua comunque

windows.desktop.shared.copy.journalmarkunverifiedtitle = non è stato possibile verificare

windows.desktop.shared.copy.keepboth = tieni entrambi

windows.desktop.shared.copy.listunavailablebody = non è stato possibile caricare i dispositivi nel tuo diario. riprova quando il tuo diario è raggiungibile.

windows.desktop.shared.copy.listunavailabletitle = dispositivi non disponibili

windows.desktop.shared.copy.offlinebody = tutto ciò che è in attesa di invio resta su questo PC. riprova quando il tuo diario è raggiungibile.

windows.desktop.shared.copy.pickerempty = nessun altro dispositivo accoppiato

windows.desktop.shared.copy.replaceconfirmbody = questo PC continua il proprio nome e la propria cronologia. il dispositivo selezionato perderà l'accesso al tuo diario.

windows.desktop.shared.copy.replaceconfirmtitle = sostituisci "{ $selecteddevicelabel }"?

windows.desktop.shared.copy.replaceconfirmtitlefallback = sostituisci il dispositivo selezionato?

windows.desktop.shared.copy.replacedevice = sostituisci il dispositivo

windows.desktop.shared.copy.replaceofferbody = puoi tenere entrambi, oppure scegliere un dispositivo che questo sostituisce.

windows.desktop.shared.copy.replaceoffertitle = questo sostituisce uno dei tuoi dispositivi?

windows.desktop.shared.copy.settingsjournalopen = apri journal

windows.desktop.shared.copy.settingslogexportsave = salva una copia…

windows.desktop.shared.copy.settingslogexporttitle = registri recenti

windows.desktop.shared.copy.settingstabattentiona11y = richiede attenzione

windows.desktop.shared.copy.settingstabupdatesdonea11y = solstone è aggiornato

windows.desktop.shared.copy.setupapplabel = l'app di solstone

windows.desktop.shared.copy.setupback = indietro

windows.desktop.shared.copy.setupconnectedtoyourjournal = connesso al tuo diario

windows.desktop.shared.copy.setupcontinue = continua

windows.desktop.shared.copy.setupjournalcardheading = il tuo diario

windows.desktop.shared.copy.setupmicrophoneheadline = condividi le tue conversazioni

windows.desktop.shared.copy.setupopenyourjournal = apri il tuo diario

windows.desktop.shared.copy.setupscreenframecaption = il tuo schermo, un momento fa

windows.desktop.shared.copy.setupwelcomebody2 = il tuo diario è sempre privato, solo tuo.

windows.desktop.shared.copy.setupwindowtitle = configura solstone

windows.desktop.shared.copy.sourcesbrowserconnectednow = connesso ora

windows.desktop.shared.copy.sourcesbrowserdiscard = scarta le pagine del browser in attesa

windows.desktop.shared.copy.sourcesbrowserdiscardcommit = scarta le pagine

windows.desktop.shared.copy.sourcesbrowserdiscardconfirm = le pagine del browser ancora in attesa di invio saranno rimosse da questo PC.

windows.desktop.shared.copy.sourcesbrowserheld = le nuove pagine del browser non vengono prese in questo momento.

windows.desktop.shared.copy.sourcesbrowserpages = pagine del browser

windows.desktop.shared.copy.storageunavailablebody = questo PC non ha potuto leggere la connessione salvata. tutto ciò che è in attesa di invio non è stato rimosso.

windows.desktop.shared.copy.storageunavailabletitle = connessione salvata non disponibile

windows.desktop.shared.copy.targetmissingbody = quel dispositivo non è più nell'elenco del tuo diario. scegli un altro dispositivo, oppure tieni entrambi.

windows.desktop.shared.copy.targetmissingtitle = dispositivo non più disponibile

windows.desktop.shared.copy.technicaldetails = dettagli tecnici

windows.desktop.shared.copy.text = { $value } · { $value2 }

windows.desktop.shared.copy.tryagain = riprova

windows.desktop.shared.settings.add = aggiungi

windows.desktop.shared.settings.excludedapps = app escluse

windows.desktop.shared.settings.gethelp = ottieni aiuto

windows.desktop.shared.settings.help = aiuto

windows.desktop.shared.settings.reportaproblem = segnala un problema

windows.desktop.shared.settings.severity = ok

windows.desktop.shared.settings.textcontent = riprova

windows.desktop.shared.settings.updates = aggiornamenti

windows.desktop.shared.settings.windowtitles = titoli delle finestre

windows.desktop.shared.setup.cancel = annulla

windows.desktop.shared.setup.pair = accoppia

windows.desktop.shared.setup.startatlogin = avvia all'accesso

windows.desktop.shared.setup.wizardpausefailure = pausa

windows.desktop.shared.setup.wizardpausefailure2 = riprendi

windows.desktop.shared.updates.automaticupdates = aggiornamenti automatici

windows.desktop.shared.updates.checkforupdatesautomatically = cerca aggiornamenti automaticamente

windows.desktop.shared.updates.checknow = cerca ora

windows.desktop.shared.updates.dismiss = ignora

windows.desktop.shared.updates.download = scarica

windows.desktop.shared.updates.downloadupdatesinthebackground = scarica gli aggiornamenti in secondo piano

windows.desktop.shared.updates.everyday = ogni giorno

windows.desktop.shared.updates.everymonth = ogni mese

windows.desktop.shared.updates.everyweek = ogni settimana

windows.desktop.shared.updates.howoften = ogni quanto

windows.desktop.shared.updates.nevercheckedforupdates = aggiornamenti mai cercati

windows.desktop.shared.updates.textcontent = leggi le note complete online

windows.desktop.shared.updates.whatsnew = novità

windows.exclusions.activity = { $count ->
    [one] { $display } fotogramma tenuto fuori dal tuo diario in questa sessione
    [many] { $display } di fotogrammi tenuti fuori dal tuo diario in questa sessione
   *[other] { $display } fotogrammi tenuti fuori dal tuo diario in questa sessione
}

windows.exclusions.activityredacted = { $count ->
    [one] { $display } fotogramma tenuto fuori dal tuo diario in questa sessione · { $more } in più sono arrivati nel tuo diario con una finestra esclusa oscurata
    [many] { $display } di fotogrammi tenuti fuori dal tuo diario in questa sessione · { $more } in più sono arrivati nel tuo diario con una finestra esclusa oscurata
   *[other] { $display } fotogrammi tenuti fuori dal tuo diario in questa sessione · { $more } in più sono arrivati nel tuo diario con una finestra esclusa oscurata
}

windows.health.attentioncount = { $count ->
    [one] { $display } richiede attenzione
    [many] { $display } richiedono attenzione
   *[other] { $display } richiedono attenzione
}

windows.import.allhistory = tutta la cronologia delle importazioni nel tuo diario

windows.import.expiredword = scaduta

windows.import.holdoffline = in attesa che il tuo diario si riconnetta.

windows.import.intro = aggiunge qualsiasi file scegli qui o rilasci su questa finestra. il tuo diario capisce che cos'è.

windows.import.morehelper = guide, opzioni e cronologia delle importazioni nel tuo diario

windows.import.readytoreviewword = pronta per rivedere

windows.import.recenta11y = { $name }, nel tuo diario, { $relative }. apri nel tuo diario.

windows.import.recentmeta = nel tuo diario · { $relative }

windows.import.removereadybody = questo rimuove la copia su questo dispositivo. la copia in attesa nel tuo diario resta lì finché non la scarti o non scade.

windows.import.reviewaction = rivedi nel tuo diario

windows.import.reviewactiona11y = rivedi { $name } nel tuo diario

windows.import.reviewdetail = salvato nel tuo diario. mostra l'anteprima e conferma per iniziare.

windows.import.reviewdetailbody = salvato nel tuo diario. rivedi entro un'ora, prima che il tuo diario rimuova la copia in attesa.

windows.import.reviewexpired = la copia in attesa nel tuo diario è scaduta. questo dispositivo ha ancora il file.

windows.import.rowsending = invio al tuo diario…

windows.import.sectionreview = in attesa della tua revisione

windows.import.sendagainaction = invia di nuovo per rivedere

windows.import.sendagainactiona11y = invia di nuovo { $name } per rivedere

windows.import.statusattentionheadline = le importazioni hanno bisogno di te

windows.import.statusattentionsub = apri l'importazione per vedere perché.

windows.import.statusreviewheadline = { $count ->
    [one] { $display } importazione pronta per rivedere
    [many] { $display } di importazioni pronte per rivedere
   *[other] { $display } importazioni pronte per rivedere
}

windows.import.statusreviewsub = apri l'importazione per rivedere e iniziare.

windows.import.tilea11y = importazione, { $state }. { $sub }.

windows.import.tilereadycount = { $display } pronti nel tuo diario

windows.import.tilesendingcount = { $count ->
    [one] { $display } file
    [many] { $display } di file
   *[other] { $display } file
}

windows.import.tilewaitingcount = { $display } in attesa

windows.import.tilewaitingjournal = in attesa del tuo diario

windows.logs.availablelogsareshownbelowcheckthe = i registri disponibili sono sotto. controlla le note per i vuoti prima di salvare una copia.

windows.logs.copysaved = copia salvata.

windows.logs.couldntreadthelogfilestry = non è stato possibile leggere i file di registro. riprova.

windows.logs.nologfilesareavailableyettry = non ci sono ancora file di registro. riprova dopo aver usato l'app.

windows.logs.norecentlogsareavailableyettry = non ci sono ancora registri recenti. riprova dopo aver usato l'app.

windows.logs.readthelogsbelowthenchoosewhether = leggi i registri sotto, poi scegli se salvare una copia.

windows.logs.textcontent = leggi i registri recenti dell'app di solstone prima di salvare una copia per una segnalazione al supporto. da qui non viene inviato niente.

windows.logs.textcontent2 = leggi i registri recenti

windows.logs.textcontent3 = lettura dei registri…

windows.logs.textcontent4 = non è stato possibile leggere i registri. riprova.

windows.logs.textcontent5 = non è stato possibile salvare la copia. prova a salvare in un'altra cartella.

windows.notice.everythingsavedonthispcisgoing = tutto ciò che è salvato su questo PC sta andando nel tuo diario.

windows.notice.everythingyouturnedonisgoinginto = tutto ciò che hai attivato sta di nuovo andando nel tuo diario.

windows.notice.intakeisrunningagain = l'ingresso è di nuovo in esecuzione

windows.notice.intakeranintoaproblem = l'ingresso ha avuto un problema

windows.notice.itsgoingintoyourjournalagain = sta di nuovo andando nel tuo diario.

windows.notice.solstoneisstilloneverythingissaved = solstone è ancora attivo. tutto è salvato su questo PC e va nel tuo diario quando è di nuovo raggiungibile.

windows.notice.theisback = { $value } è di nuovo disponibile

windows.notice.theneedsyourpermission = { $value } ha bisogno del tuo permesso

windows.notice.thesolstoneiconinthetaskbarsays = l'icona di solstone nella barra delle applicazioni dice che cosa non va.

windows.notice.windowsprivacysettingsareblockingitturn = le impostazioni di privacy di windows lo bloccano. attiva l'accesso a { $value } per le app desktop, e va di nuovo nel tuo diario.

windows.notice.yourjournalisbackinreach = il tuo diario è di nuovo raggiungibile

windows.notice.yourjournalisoutofreach = il tuo diario non è raggiungibile

windows.obsidian.chooseobsidianvault = scegli il tuo vault di Obsidian

windows.obsidian.vault = vault

windows.obsidian.warning = il tuo diario ha già un vault chiamato { $vault } da un altro dispositivo. se continui, le modifiche di entrambi i dispositivi vanno nel tuo diario.

windows.obsidian.warningdate = il tuo diario ha già un vault chiamato { $vault } da un altro dispositivo, aggiornato l'ultima volta { $date }. se continui, le modifiche di entrambi i dispositivi vanno nel tuo diario.

windows.obsidian.warningdevice = il tuo diario ha già un vault chiamato { $vault } da { $device }. se continui, le modifiche di entrambi i dispositivi vanno nel tuo diario.

windows.obsidian.warningdevicedate = il tuo diario ha già un vault chiamato { $vault } da { $device }, aggiornato l'ultima volta { $date }. se continui, le modifiche di entrambi i dispositivi vanno nel tuo diario.

windows.pill.held = conferma la marca

windows.pill.heldwaiting = conferma la marca · { $waiting } in attesa

windows.pill.offline = non in linea

windows.pill.offlinewaiting = non in linea · { $waiting } in attesa

windows.pill.readytoreview = { $display } pronti per rivedere

windows.pill.refused = accoppia di nuovo

windows.pill.refusedwaiting = accoppia di nuovo · { $waiting } in attesa

windows.pill.syncpaused = sincronizzazione in pausa

windows.pill.syncpausedwaiting = sincronizzazione in pausa · { $waiting } in attesa

windows.quantity.hours = { $count ->
    [one] { $display } ora
    [many] { $display } di ore
   *[other] { $display } ore
}

windows.quantity.items = { $count ->
    [one] { $display } elemento
    [many] { $display } di elementi
   *[other] { $display } elementi
}

windows.quantity.minutes = { $count ->
    [one] { $display } minuto
    [many] { $display } di minuti
   *[other] { $display } minuti
}

windows.quantity.notes = { $count ->
    [one] { $display } nota
    [many] { $display } di note
   *[other] { $display } note
}

windows.quantity.pages = { $count ->
    [one] { $display } pagina
    [many] { $display } di pagine
   *[other] { $display } pagine
}

windows.quantity.screenshots = { $count ->
    [one] { $display } screenshot
    [many] { $display } di screenshot
   *[other] { $display } screenshot
}

windows.screenshots.bringcount = { $count ->
    [one] porta { $display } screenshot
    [many] porta { $display } di screenshot
   *[other] porta { $display } screenshot
}

windows.screenshots.cantcount = { $count ->
    [one] { $display } screenshot non può entrare, quindi resta nella cartella.
    [many] { $display } di screenshot non possono entrare, quindi restano nella cartella.
   *[other] { $display } screenshot non possono entrare, quindi restano nella cartella.
}

windows.screenshots.unreadablecount = { $count ->
    [one] non è stato possibile leggere { $display } screenshot.
    [many] non è stato possibile leggere { $display } di screenshot.
   *[other] non è stato possibile leggere { $display } screenshot.
}

windows.sharedsources.also = anche su

windows.sharedsources.labelage = { $label } · { $relative }

windows.sharedsources.labelstate = { $label } · { $state }

windows.sharedsources.labelstateage = { $label } · { $state } · { $relative }

windows.sharedsources.many = da { $count } altri dispositivi

windows.sharedsources.one = da un altro dispositivo

windows.sharedsources.stateage = { $state } · { $relative }

windows.time.checkedday = { $count ->
    [one] controllato { $display } giorno fa
    [many] controllato { $display } di giorni fa
   *[other] controllato { $display } giorni fa
}

windows.time.checkedhour = { $count ->
    [one] controllato { $display } ora fa
    [many] controllato { $display } di ore fa
   *[other] controllato { $display } ore fa
}

windows.time.checkedminute = { $count ->
    [one] controllato { $display } minuto fa
    [many] controllato { $display } di minuti fa
   *[other] controllato { $display } minuti fa
}

windows.time.nextsave = tra { $minutes }:{ $seconds }

windows.time.takenhours = { $count ->
    [one] { $display } ora fa
    [many] { $display } di ore fa
   *[other] { $display } ore fa
}

windows.tray.checkforupdates = cerca aggiornamenti…

windows.tray.hrmin = { $count } h { $count2 } min

windows.tray.lessthanaminute = meno di un minuto

windows.tray.needsarestart = serve un riavvio

windows.tray.onbut = attivo, ma { $needs }

windows.tray.onconnectedtoyourjournal = attivo, connesso al tuo diario

windows.tray.onnotconnectedtoajournal = attivo, non connesso a un diario

windows.tray.onsavedonthispc = attivo, salvato su questo PC

windows.tray.pausedleft = in pausa, { $value } rimasti

windows.tray.solstonewaitingforyoutoconfirmyour = solstone · in attesa che tu confermi la marca del tuo diario

windows.tray.starting = avvio…

windows.tray.storagefaultforowner = non è stato possibile salvare su questo PC

windows.tray.text = { $tooltip } { $notice }

windows.tray.theneedspermissioninwindowssettings = { $value } ha bisogno del permesso nelle impostazioni di windows

windows.tray.thestoppedgoingintoyourjournal = { $value } ha smesso di andare nel tuo diario

windows.tray.unknownjournalseenat = diario sconosciuto visto su { $name }

windows.tray.unknownjournalseenthroughtherelay = diario sconosciuto visto attraverso il relay

windows.tray.updateavailable = aggiornamento disponibile

windows.tray.updatecheckfailed = non è stato possibile cercare aggiornamenti

windows.tray.updateready = aggiornamento pronto · si installa quando esci e riapri

windows.ui.accesswasdenied = l'accesso è stato negato.

windows.ui.accesswasdeniedcheckwindowspermissionsfor = l'accesso è stato negato. controlla i permessi di windows per questa fonte, poi riprova.

windows.ui.activex474d1b75 = attivo:

windows.ui.activex68f50a9b = { $leading }· attivo

windows.ui.activex96879611 = attivo

windows.ui.activity = attività

windows.ui.activityisstillon = l'attività è ancora attiva.

windows.ui.addappnameswindowtitlesandpage = aggiunge al tuo diario i nomi delle app, i titoli delle finestre e gli indirizzi delle pagine, e se il tuo stato è attivo o inattivo, o se questo PC è bloccato. è attiva mentre lo schermo è attivo, e va in pausa con lui. gli indirizzi delle pagine includono il sito e il percorso, senza niente dopo ? o #.

windows.ui.addmore = aggiungi altro

windows.ui.addspdfsaudioimagesandtextyouchoosehereordropont = scegli i file da portare nel tuo diario, oppure rilasciali su questa finestra.

windows.ui.addthesolstoneextension = aggiungi l'estensione di solstone.

windows.ui.addto = aggiungi a { $value }

windows.ui.aglobalshortcuttopauseandresume = una scorciatoia globale per mettere in pausa e riprendere l'ingresso da qualsiasi punto.

windows.ui.allcaughtup = in pari

windows.ui.allofit = tutto

windows.ui.allsourceson = tutte le fonti attive

windows.ui.alouderinputforquietmicrophoneschanges = un ingresso più forte per i microfoni bassi. le modifiche si applicano subito. un guadagno più alto può far entrare più rumore di fondo in una stanza silenziosa.

windows.ui.alreadyonhome = già su inizio

windows.ui.anewlinkfromyourjournalcanbepastedhere = qui si può incollare un nuovo link dal tuo diario.

windows.ui.anotherpairinglinkcouldnotbeadded = non è stato possibile aggiungere un altro link di accoppiamento.

windows.ui.april = aprile

windows.ui.arg0 = { $leading }· { $arg0 }

windows.ui.arg0arg1 = { $arg0 }
    { $arg1 }

windows.ui.arg0arg1arg2 = { $arg0 } { $arg1 }, { $arg2 }

windows.ui.arg0bytes = { $arg0 } byte

windows.ui.arg0gb = { $arg0 } GB

windows.ui.arg0hr = { $arg0 } h

windows.ui.arg0hrleft = { $arg0 } h rimanenti

windows.ui.arg0kb = { $arg0 } KB

windows.ui.arg0kbx75b11ba1 = { $arg0 } KB

windows.ui.arg0mb = { $arg0 } MB

windows.ui.arg0mbxff882f00 = { $arg0 } MB

windows.ui.arg0min = { $arg0 } min

windows.ui.arg0minago = { $arg0 } min fa

windows.ui.arg0minleft = { $arg0 } min rimasti

windows.ui.arg0needsattention = { $arg0 } richiede attenzione

windows.ui.arg0ofarg1inyourjournal = { $arg0 } di { $arg1 } nel tuo diario

windows.ui.arg0ofarg1screenshotsinyourjournal = { $arg0 } di { $arg1 } screenshot nel tuo diario

windows.ui.arg0tb = { $arg0 } TB

windows.ui.arg0x211b8ba1 = { $arg0 }%

windows.ui.ascreenshotalreadyinyourjournalstaysthereifyoude = uno screenshot già nel tuo diario resta lì se lo elimini dalla cartella.

windows.ui.ascreenshotthatsonlycopiedtotheclipboardisntsave = uno screenshot copiato solo negli appunti non viene salvato, quindi non entra.

windows.ui.asegmentisa5minutelocal = un segmento è un pacchetto locale di 5 minuti che resta qui finché il tuo diario non lo riceve.

windows.ui.attentionneeded = serve attenzione

windows.ui.audio = audio

windows.ui.august = agosto

windows.ui.automaticchecksareoff = i controlli automatici sono disattivi

windows.ui.automaticchecksareon = i controlli automatici sono attivi

windows.ui.awindowthatclosesormovescan = una finestra che si chiude o si sposta può comparire per al massimo un fotogramma prima che l'esclusione si applichi.

windows.ui.backgrounddatauserestricted = l'uso dei dati in secondo piano è limitato

windows.ui.browsers = browser

windows.ui.browserstateunavailable = stato del browser non disponibile

windows.ui.change = cambia

windows.ui.checkedjustnow = controllato proprio ora

windows.ui.checkforupdates = cerca aggiornamenti

windows.ui.checking = controllo…

windows.ui.checkingforupdates = ricerca di aggiornamenti…

windows.ui.checkingword = controllo…

windows.ui.checkingyourjournal = controllo del tuo diario.

windows.ui.chooseafolder = scegli una cartella.

windows.ui.chooseafolderxdbec742d = scegli una cartella…

windows.ui.chooseanapptoexclude = scegli un'app da escludere

windows.ui.choosearunningapp = scegli un'app in esecuzione…

windows.ui.choosefiles = scegli i file…

windows.ui.choosefolder = scegli una cartella

windows.ui.choosehowfarbacktostartthenbringthemin = scegli da quanto indietro iniziare, poi portali.

windows.ui.choosethefolderyourscreenshotssaveto = scegli la cartella in cui si salvano i tuoi screenshot

windows.ui.choosewhatstaysoutofyourjournal = scegli che cosa resta fuori dal tuo diario. le modifiche hanno effetto subito.

windows.ui.claimednotverified = dichiarato, non verificato

windows.ui.clickthenpressyourshortcut = clicca, poi premi la tua scorciatoia

windows.ui.closenavigation = chiudi la navigazione

windows.ui.computersound = audio del computer

windows.ui.computersoundlevel = livello dell'audio del computer

windows.ui.confirmthemark = conferma la marca

windows.ui.confirmthemarkarg0waiting = conferma la marca · { $arg0 } in attesa

windows.ui.confirmyourjournalsmarktoopen = conferma la marca del tuo diario per aprirlo

windows.ui.connectajournal = connetti un diario

windows.ui.connected = connesso

windows.ui.connectedto = connesso a { $value }

windows.ui.connecting = connessione…

windows.ui.connectingx3d6df245 = connessione in corso

windows.ui.connection = connessione

windows.ui.connecttoyourjournal = connettiti al tuo diario

windows.ui.connectyourjournal = connetti il tuo diario

windows.ui.continuethisconnection = continua questa connessione

windows.ui.copied = copiato

windows.ui.copythefulllinkfromyourchatandopenitagain = copia il link completo dalla tua chat e aprilo di nuovo.

windows.ui.couldnotpausetryagain = non è stato possibile mettere in pausa. riprova.

windows.ui.couldnotresumetryagain = non è stato possibile riprendere. riprova.

windows.ui.couldnotsavethischoiceitmay = non è stato possibile salvare questa scelta. può cambiare dopo un riavvio. riprova.

windows.ui.couldnotsavethisdismissaltryagain = non è stato possibile salvare la scelta di ignorare. riprova.

windows.ui.couldnotsaveturningthisoffitisoffnowbutmayturnon = non è stato possibile salvare la disattivazione. è disattivo ora, ma può attivarsi di nuovo dopo un riavvio. riprova.

windows.ui.couldnotsaveturningthisonitstaysofftryagain = non è stato possibile salvare l'attivazione. resta disattivo. riprova.

windows.ui.couldnotsaveyoursetupprogresstry = non è stato possibile salvare l'avanzamento della configurazione iniziale. riprova.

windows.ui.couldnotupdatestartatlogin = non è stato possibile aggiornare l'avvio all'accesso

windows.ui.couldntcheckforupdates = non è stato possibile cercare aggiornamenti

windows.ui.couldntcheckforupdatessolstonewas = non è stato possibile cercare aggiornamenti. solstone { $v } è stato trovato prima

windows.ui.couldntcountyourscreenshots = non è stato possibile contare i tuoi screenshot.

windows.ui.couldntdownloadsolstone = non è stato possibile scaricare solstone { $value }

windows.ui.couldntdownloadupdate = non è stato possibile scaricare l'aggiornamento

windows.ui.couldntfindascreenshotsfolderonthispcchoosethefo = non è stato possibile trovare una cartella Screenshot su questo PC. scegli la cartella in cui si salvano i tuoi screenshot.

windows.ui.couldntfinishdiscardingwhatsleft = non è stato possibile finire di scartare. ciò che resta è ancora su questo PC.

windows.ui.couldntinstallupdate = non è stato possibile installare l'aggiornamento

windows.ui.couldntloadthesolstoneapps = non è stato possibile caricare lo stato dell'app di solstone proprio ora.

windows.ui.couldntopenthestorepagein = non è stato possibile aprire la pagina dello store in { $value }.

windows.ui.couldntopenyourjournal = non è stato possibile aprire il tuo diario.

windows.ui.couldntopenyourjournaltryagain = non è stato possibile aprire il tuo diario. riprova.

windows.ui.couldntreachyourjournaloveryour = non è stato possibile connettersi al tuo diario attraverso la tua rete privata. entra nella stessa wi-fi del tuo diario, poi riprova con un nuovo link di accoppiamento dal tuo diario.

windows.ui.couldntreadimportsonthisdevicenothinghasbeenremo = non è stato possibile leggere le importazioni su questo dispositivo. non è stato rimosso niente. riavvia l'app di solstone per riprovare.

windows.ui.couldntreadyoursavedsourcechoices = non è stato possibile leggere le tue scelte delle fonti salvate. le fonti sono disattive. scegli di nuovo le tue fonti nelle impostazioni.

windows.ui.couldntregisterthatshortcut = non è stato possibile registrare quella scorciatoia

windows.ui.couldntremovethistryagain = non è stato possibile rimuoverlo. riprova.

windows.ui.couldntturntryagain = non è stato possibile attivarlo. riprova.

windows.ui.couldntverifyjournalsonothing = non è stato possibile verificare il tuo diario, quindi non si è connesso niente. riprova, oppure incolla un link di accoppiamento.

windows.ui.dayago = { $count ->
    [one] { $count } giorno fa
    [many] { $count } di giorni fa
   *[other] { $count } giorni fa
}

windows.ui.december = dicembre

windows.ui.delivered = { $value } consegnato

windows.ui.deliveredpending = { $value } consegnato · { $value2 } in attesa

windows.ui.desktopactivity = attività del desktop

windows.ui.devicechoice = scelta del dispositivo

windows.ui.devicepriority = priorità dei dispositivi

windows.ui.disable = disattiva

windows.ui.dontstartatlogin = non avviare all'accesso

windows.ui.downloaddeferredconnectionstatusunknown = scaricamento rinviato: stato della connessione sconosciuto

windows.ui.downloaddeferredmeteredconnection = scaricamento rinviato su una connessione a consumo

windows.ui.downloaddeferredwhileroaming = scaricamento rinviato durante il roaming

windows.ui.downloadingsolstone = scaricamento di solstone { $v }

windows.ui.downloadthelatestfromsolstoneappdownloadwindows = scarica l'ultima versione da solstone.app/download/windows

windows.ui.edgeandfirefoxsettoenglish = Edge e Firefox impostati sull'inglese

windows.ui.enable = attiva

windows.ui.enablethepauseresumeshortcut = attiva la scorciatoia pausa / riprendi

windows.ui.errorword = errore

windows.ui.esctocancel = esc per annullare

windows.ui.everypicturesavedinarg0comesinnotonlyyourscreens = ogni immagine salvata in { $arg0 } entra, non solo i tuoi screenshot.

windows.ui.everythingisinyourjournal = tutto è nel tuo diario

windows.ui.exclude = escludi

windows.ui.exclusionactivity = attività di esclusione

windows.ui.failedtoacquiresetuplock = non è stato possibile acquisire il blocco della configurazione iniziale

windows.ui.february = febbraio

windows.ui.file = file

windows.ui.files = file

windows.ui.finishconnecting = finisci la connessione

windows.ui.finishconnectingtoyourjournal = finisci la connessione al tuo diario

windows.ui.finishthecurrentconnectionthentryagain = finisci la connessione attuale, poi riprova.

windows.ui.folder = cartella

windows.ui.foundjournalpc = trovato il tuo diario su questo PC

windows.ui.from = da { $value }

windows.ui.frombeforetoday = da prima di oggi

windows.ui.gettingready = preparazione…

windows.ui.gettingreadyconnectingtoyourjournal = preparazione. connessione al tuo diario.

windows.ui.gettingthisdeviceready = preparazione di questo PC

windows.ui.globalshortcut = scorciatoia globale

windows.ui.goesalongwithit = va insieme

windows.ui.goodafternoon = buon pomeriggio

windows.ui.goodevening = buonasera

windows.ui.goodmorning = buongiorno

windows.ui.holdoneormoreofctrlalt = tieni premuto uno o più tra Ctrl, Alt, Shift o Win e premi un tasto.

windows.ui.home = inizio

windows.ui.homex4e7a5cec = ‹ inizio

windows.ui.hourago = { $count ->
    [one] { $count } ora fa
    [many] { $count } di ore fa
   *[other] { $count } ore fa
}

windows.ui.howoftentocheckforupdates = ogni quanto cercare aggiornamenti

windows.ui.hrlefthrminleftboth = { $count } h { $count2 } min rimanenti

windows.ui.idle = inattivo

windows.ui.ifyouhaventyetaddthe = se non l'hai ancora fatto, aggiungi l'estensione di solstone a un browser su questo PC. ciò che condividi va nel tuo diario.

windows.ui.import = importazione

windows.ui.importswaitonthisdeviceuntilyouconnectajournal = le importazioni aspettano su questo dispositivo finché non connetti un diario.

windows.ui.importswaituntilyouconfirmyourjournalsmark = le importazioni aspettano finché non confermi la marca del tuo diario.

windows.ui.importswaituntilyourjournalisupdated = le importazioni aspettano finché il tuo diario non è aggiornato.

windows.ui.inputgain = guadagno di ingresso

windows.ui.installing = installazione…

windows.ui.installingsolstone = installazione di solstone { $v }…

windows.ui.inyourjournal = nel tuo diario.

windows.ui.inyourjournalchoosepairadeviceandpastethelinkher = nel tuo diario, scegli "accoppia un dispositivo" e incolla il link qui.

windows.ui.itinstallsthenexttimethesolstoneappstartsincludi = si installa la prossima volta che l'app di solstone si avvia, anche quando accedi a windows

windows.ui.itslargerthanyourjournaltakes = è più grande di quanto il tuo diario prende

windows.ui.january = gennaio

windows.ui.journaldetails = dettagli del diario

windows.ui.journallastreached = ultima connessione al diario

windows.ui.journalneedsupdatebeforecantake = il tuo diario ha bisogno di un aggiornamento prima di poter prendere queste note. sono in attesa su questo computer.

windows.ui.journalsync = sincronizzazione del diario

windows.ui.journalupdateneeded = serve un aggiornamento del diario

windows.ui.journalversion = versione del diario

windows.ui.july = luglio

windows.ui.june = giugno

windows.ui.keeponwhilepausedorscreenis = tieni attivo durante la pausa o quando lo schermo è disattivo

windows.ui.keepprivatewindowsoutofyourjournal = tieni le finestre private fuori dal tuo diario: Edge e Firefox impostati sull'inglese

windows.ui.keptonthispc = tenuto su questo PC

windows.ui.keptonthispcnotreachingyour = tenuto su questo PC, non si connette al tuo diario in questo momento

windows.ui.label = non accoppiato. accoppia per sincronizzare il tuo diario

windows.ui.lastchecked = ultimo controllo

windows.ui.lastreached = ultima connessione

windows.ui.lasttakenin = ultimo ingresso

windows.ui.leftout = lasciato fuori

windows.ui.lessthanaminuteleft = meno di un minuto rimasto

windows.ui.madebysolpbc = fatto da sol pbc.

windows.ui.march = marzo

windows.ui.mark = marca

windows.ui.markunavailable = marca·non disponibile

windows.ui.markunavailablex5acbd11e = marca · non disponibile

windows.ui.may = maggio

windows.ui.microphone = microfono

windows.ui.microphonelevel = livello del microfono

windows.ui.microphones = microfoni

windows.ui.minuteago = { $count ->
    [one] { $count } minuto fa
    [many] { $count } di minuti fa
   *[other] { $count } minuti fa
}

windows.ui.moreaboutthis = altro su questo

windows.ui.moreimports = altre importazioni

windows.ui.movedown = sposta { $name } in basso

windows.ui.moveup = sposta { $name } in alto

windows.ui.n1passwordbitwardenandtheotherpasswordmanager = 1Password, Bitwarden e le altre app di gestione password elencate nelle impostazioni sono lasciate fuori, ma un gestore di password dentro il browser no. puoi mettere in pausa in qualsiasi momento.

windows.ui.needsattentionword = richiede attenzione

windows.ui.needspermission = serve il permesso

windows.ui.next = avanti

windows.ui.nextsave = prossimo salvataggio

windows.ui.nobrowserconnectednow = nessun browser connesso ora

windows.ui.nocredentialaccess = nessun accesso alle credenziali

windows.ui.noexclusionsactive = nessuna esclusione attiva

windows.ui.noidentitypresented = nessuna identità presentata

windows.ui.noinputdevice = nessun dispositivo di ingresso

windows.ui.nokeywordsyet = ancora nessuna parola chiave

windows.ui.nolocaljournaltoconnectto = nessun diario locale a cui connettersi

windows.ui.nomicrophonefound = nessun microfono trovato.

windows.ui.nomicrophoneinputdevices = nessun dispositivo di ingresso microfono

windows.ui.nomicrophoneisavailableconnectorenableoneinwindo = nessun microfono è disponibile. connetti o attivalo nelle impostazioni audio di windows. l'app di solstone inizia a usarlo da sola.

windows.ui.nonedisplay = niente

windows.ui.nootherappsrunning = nessun'altra app in esecuzione

windows.ui.noroommorerightnownew = non c'è spazio per altre in questo momento. le nuove pagine vengono prese di nuovo quando c'è spazio.

windows.ui.noshortcutset = nessuna scorciatoia impostata

windows.ui.nosoundoutputfound = nessuna uscita audio trovata.

windows.ui.nosoundoutputisavailableconnectorenableoneinwind = nessuna uscita audio è disponibile. connetti o attivala nelle impostazioni audio di windows. l'app di solstone inizia a usarla da sola.

windows.ui.not = non

windows.ui.notanswered = nessuna risposta

windows.ui.notavailable = non disponibile

windows.ui.notavailablerightnow = non disponibile in questo momento.

windows.ui.notcheckedforupdatesyet = aggiornamenti non ancora cercati

windows.ui.notesjournal = { $value } di { $value2 } note nel tuo diario

windows.ui.notgoingtoyourjournalrightnoweachonesayswhy = non sta andando nel tuo diario in questo momento. ognuno dice perché.

windows.ui.nothingexcludedyet = ancora niente di escluso

windows.ui.nothingfrombeforetoday = niente da prima di oggi

windows.ui.nothingwassavedonthisdevice = non è stato salvato niente su questo dispositivo.

windows.ui.nothingyet = ancora niente

windows.ui.nothingyetxec5a8da6 = ancora niente.

windows.ui.notonthiscomputer = non su questo computer

windows.ui.notreachingyourjournalrightnow = non si connette al tuo diario in questo momento

windows.ui.notreported = non segnalato

windows.ui.notyet = non ancora

windows.ui.nousagedataiseversentupdate = non viene mai inviato alcun dato di utilizzo. i controlli degli aggiornamenti scaricano solo il manifest della versione.

windows.ui.november = novembre

windows.ui.october = ottobre

windows.ui.offline = non in linea

windows.ui.offlinearg0waiting = non in linea · { $arg0 } in attesa

windows.ui.onlywhatthesolstoneappcantellisascreenshotcomesi = entra solo ciò che l'app di solstone riconosce come uno screenshot.

windows.ui.onthesitesyouallowthesolstoneextensiontakesinthe = sui siti che consenti, l'estensione di solstone prende il testo della pagina e la disposizione approssimativa, e tutto va nel tuo diario.

windows.ui.onthisdevice = su questo dispositivo

windows.ui.openfolder = apri la cartella

windows.ui.opennavigation = apri la navigazione

windows.ui.opensettings = apri le impostazioni

windows.ui.opensoundsettings = apri le impostazioni audio

windows.ui.openwindowssettings = apri le impostazioni di windows

windows.ui.otherfilesinarg0staywheretheyare = gli altri file in { $arg0 } restano dove sono.

windows.ui.pageaddresses = indirizzi delle pagine

windows.ui.pageaddressesedgeandchrome = indirizzi delle pagine (Edge e Chrome)

windows.ui.pairagain = accoppia di nuovo

windows.ui.pairedwith = accoppiato con { $value }

windows.ui.pairing = accoppiamento

windows.ui.pairingcancellednothingthispchaskept = accoppiamento annullato. incolla un link nuovo dal tuo diario quando vuoi.

windows.ui.pairingdidntgothroughgeta = l'accoppiamento non è riuscito. ottieni un nuovo link di accoppiamento dal tuo diario e riprova.

windows.ui.pairinglinkfromyourjournal = link di accoppiamento dal tuo diario

windows.ui.pairthispctoyourjournalto = accoppia questo PC al tuo diario per aprirlo

windows.ui.pairtodelivertoyourjournal = accoppia per consegnare al tuo diario

windows.ui.pairwithyourjournalfirst = accoppia prima il tuo diario

windows.ui.partvaultcantberead = una parte di questo vault non si può leggere. controlla i permessi delle cartelle al suo interno. il resto del vault continua ad andare nel tuo diario.

windows.ui.paused = in pausa, { $count }

windows.ui.pausedarg0minleft = in pausa, { $arg0 } min rimasti

windows.ui.pausedlessthanaminuteleft = in pausa, meno di un minuto rimasto

windows.ui.pauseduntilyouresume = in pausa finché non riprendi

windows.ui.pausedwiththerestofthesolstone = in pausa con il resto dell'app di solstone. la pausa non trattiene ciò che è già stato preso.

windows.ui.pausedword = in pausa

windows.ui.pausedxa7a9dc5b = in pausa

windows.ui.pauseresume = pausa / riprendi

windows.ui.pausesallsources = mette in pausa tutte le fonti.

windows.ui.pending = { $value } in attesa

windows.ui.placeholder = incolla un link di accoppiamento dal tuo diario

windows.ui.placeholder2 = una parola chiave, es. banca

windows.ui.presented = presentato

windows.ui.pressyourshortcut = premi la tua scorciatoia…

windows.ui.previewpaused = anteprima in pausa

windows.ui.privacy = privacy

windows.ui.privatewindows = finestre private

windows.ui.progresstext = passaggio { $stepnum } di { $totalsteps }

windows.ui.quitsolstone = esci da solstone

windows.ui.readytosetupword = pronto per la configurazione

windows.ui.recent = recenti

windows.ui.relaunchtoinstall = riapri per installare

windows.ui.remove = rimuovi { $value }

windows.ui.removearg0 = rimuovi { $arg0 }?

windows.ui.removex7e5608ab = rimuovi

windows.ui.restartintake = riavvia l'ingresso

windows.ui.retrying = { $value } nuovo tentativo

windows.ui.saveacopyofrecentlogs = salva una copia dei registri recenti

windows.ui.savedonthisdevicearg0 = salvato su questo dispositivo: { $arg0 }

windows.ui.savingonthisdevicearg0ofarg1 = salvataggio su questo dispositivo: { $arg0 } di { $arg1 }

windows.ui.screen = schermo

windows.ui.screenalsoaddstheappsyouuse = lo schermo aggiunge anche le app che usi, con i titoli delle finestre e gli indirizzi delle pagine, al tuo diario. puoi disattivarlo nelle impostazioni.

windows.ui.screenarg0computersoundarg1micarg2desktopactivit = schermo: { $arg0 } · audio del computer: { $arg1 } · microfono: { $arg2 } · attività del desktop: { $arg3 }

windows.ui.screenpreview = anteprima dello schermo

windows.ui.screenshots = screenshot

windows.ui.screenshotsalreadyinyourjournalarentbroughtinaga = gli screenshot già nel tuo diario non vengono portati di nuovo.

windows.ui.screenshotsyoutakewhileitspauseddontcomein = gli screenshot che fai mentre è in pausa non entrano.

windows.ui.segmentsthathavenotreachedyourjournal = i segmenti che non sono ancora arrivati nel tuo diario non vengono mai eliminati.

windows.ui.selecteddevice = dispositivo selezionato

windows.ui.sendingtoyourjournal = invio al tuo diario.

windows.ui.sendingword = invio

windows.ui.september = settembre

windows.ui.set = imposta

windows.ui.settingupword = configurazione in corso

windows.ui.setupconnectionchanged = la connessione della configurazione iniziale è cambiata

windows.ui.setupisbusytryagainina = la configurazione iniziale è occupata. riprova tra un momento.

windows.ui.setupprogresscouldnotbesaved = non è stato possibile salvare l'avanzamento della configurazione iniziale

windows.ui.setupsequenceisunavailableinthisbuild = la sequenza della configurazione iniziale non è disponibile in questa build

windows.ui.setupunavailable = configurazione iniziale non disponibile

windows.ui.sharesoundfromthiscomputer = condividi l'audio di questo PC

windows.ui.shareyourscreen = condividi il tuo schermo

windows.ui.shortcut = scorciatoia

windows.ui.shortcutsetbutturnedoff = scorciatoia impostata ma disattivata

windows.ui.skip = salta

windows.ui.sofar = finora

windows.ui.solstoneappcantreadvault = l'app di solstone non può leggere questo vault. controlla che la sua cartella ci sia e che l'unità su cui si trova sia connessa. niente cambia nel tuo diario mentre non è raggiungibile.

windows.ui.solstoneappcouldntsaveits = l'app di solstone non ha potuto salvare il punto in cui era arrivata per questo vault. controlla che questo computer abbia spazio libero.

windows.ui.solstoneappdidntfindvault = l'app di solstone non ha trovato un vault su questo computer. scegli invece la sua cartella.

windows.ui.solstoneapptakesnotesonevault = l'app di solstone prende le note di un vault su questo computer. tutte vanno nel tuo diario, poi ognuna quando cambia. il tuo vault resta com'è.

windows.ui.solstonearg0installsthenexttimethesolstoneappsta = solstone { $arg0 } si installa la prossima volta che l'app di solstone si avvia, anche quando accedi a windows

windows.ui.solstoneisavailable = solstone { $v } è disponibile

windows.ui.solstoneisreadytoinstall = solstone { $v } è pronto per l'installazione

windows.ui.solstonereadyinstall = solstone è pronto per l'installazione

windows.ui.somenotescouldntgointo = alcune note non hanno potuto andare nel tuo diario. l'app di solstone le riprova al prossimo avvio.

windows.ui.somethingansweredpcatisnt = qualcosa ha risposto a questo PC a { $value }, e non è il diario con cui questo PC è accoppiato. può essere un altro dispositivo che ora usa quell'indirizzo, oppure il tuo diario, se la sua identità è stata reimpostata. se la marca sotto "che cosa ha risposto" corrisponde a quella che il tuo diario mostra ora, accoppia di nuovo questo PC.

windows.ui.somethingansweredpcthroughrelayisn = qualcosa ha risposto a questo PC attraverso il relay, e non è il diario con cui questo PC è accoppiato. è meno certo di una connessione diretta.

windows.ui.somethingotherthanyourjournalansweredcompare = ha risposto qualcosa al posto del tuo diario

windows.ui.somethingyouwereaddingdidntfinishsavingadditagai = qualcosa che stavi aggiungendo non ha finito di salvarsi. aggiungilo di nuovo.

windows.ui.sourcesetupisunfinished = la configurazione iniziale della fonte non è finita

windows.ui.startfromtoday = inizia da oggi

windows.ui.state = condizione

windows.ui.status = stato

windows.ui.storage = spazio

windows.ui.storagelocation = posizione di archiviazione

windows.ui.storeappsallappsfromthemicrosoft = app dello store (tutte). le app del Microsoft Store condividono un solo elemento, quindi escluderlo le esclude tutte

windows.ui.storedlocally = { $value } conservato in locale

windows.ui.storedonthispc = conservato su questo PC

windows.ui.sync = sincronizza

windows.ui.text = { $name }. { $value2 }

windows.ui.text3 = { $name }, { $name2 } · { $value3 } { $value4 }

windows.ui.text6 = { $status }: { $reason }

windows.ui.textcontent = è arrivato un altro link di accoppiamento: usalo o ignoralo.

windows.ui.textcontent2 = accoppiamento…

windows.ui.textcontent4 = cancella la scorciatoia

windows.ui.textcontent5 = { $level }×

windows.ui.textfilestxt = file di testo (*.txt)

windows.ui.thatcomboisinusebyanother = quella combinazione è usata da un'altra app. scegline un'altra.

windows.ui.thatdidnotfinish = non è finito

windows.ui.thatfolderisntobsidianvault = quella cartella non è un vault di Obsidian, quindi non è stato inviato niente.

windows.ui.thatfoldersnamecant = il nome di quella cartella non può essere il nome di un vault nel tuo diario. rinomina la cartella e riprova.

windows.ui.thatpairinglinkisntonethe = quel link di accoppiamento non è uno che l'app di solstone può leggere. ottieni un nuovo link di accoppiamento dal tuo diario e riprova.

windows.ui.thearg0folderisntonthispcanymore = la cartella { $arg0 } non è più su questo PC.

windows.ui.thecopyisalreadygonesothisonlyclearsitfromhere = la copia è già sparita, quindi questo la cancella solo da qui.

windows.ui.thecopyonthisdeviceismissing = la copia su questo dispositivo manca

windows.ui.thedownloaddidnotfinish = { $leading }· lo scaricamento non è finito

windows.ui.thelast30days = gli ultimi 30 giorni

windows.ui.thelast6months = gli ultimi 6 mesi

windows.ui.thelastcheckforupdatesfailed = { $leading }· l'ultima ricerca di aggiornamenti non è riuscita

windows.ui.thelastyear = l'ultimo anno

windows.ui.thepairingwindowclosedgetanew = la finestra di accoppiamento si è chiusa. ottieni un nuovo link di accoppiamento dal tuo diario, poi riprova.

windows.ui.thereisntenoughroomonthisdevice = non c'è abbastanza spazio su questo dispositivo

windows.ui.theserulesareactivenowbutcouldn = queste regole sono attive ora ma non è stato possibile salvarle, quindi possono non restare dopo un riavvio.

windows.ui.thesolstoneappusesonemicrophoneat = l'app di solstone usa un microfono alla volta. imposta quale, e quanto aumentarlo.

windows.ui.thetopenabledmicrophoneisuseduse = si usa il microfono attivo più in alto. usa ↑ ↓ per impostare l'ordine; l'app di solstone passa al successivo se uno non è disponibile.

windows.ui.theupdatecouldntbeinstalled = { $leading }· non è stato possibile installare l'aggiornamento

windows.ui.thisbuildcantupdateitself = questa build non può aggiornarsi da sola

windows.ui.thisconnectioncouldnotbesavedtry = non è stato possibile salvare questa connessione. riprova, oppure incolla un link di accoppiamento dal tuo diario.

windows.ui.thisconnectioncouldnotfinishtryagain = non è stato possibile finire questa connessione. riprova, oppure incolla un nuovo link di accoppiamento dal tuo diario.

windows.ui.thisconnectioncouldnotfinishtryagain2 = non è stato possibile finire questa connessione. riprova.

windows.ui.thiscouldntbereadadditagain = non è stato possibile leggerlo. aggiungilo di nuovo

windows.ui.thisdevicecantconvertthisphoto = questo dispositivo non può convertire questa foto

windows.ui.thisdevicecouldntopenitssaved = questo PC non ha potuto aprire la connessione salvata al tuo diario. tutto ciò che è in attesa di invio è ancora qui. accoppia di nuovo per riconnetterti.

windows.ui.thisdeviceisnotpairedwithyourjournalimportswaith = questo dispositivo non è accoppiato al tuo diario. le importazioni aspettano qui finché non connetti un diario.

windows.ui.thisdeviceisntonanetwork = questo PC non è su una rete. l'accoppiamento deve connettersi direttamente al tuo diario, quindi entra nella stessa wi-fi del tuo diario e riprova. tutto ciò che l'app di solstone ha preso è su questo PC e si sincronizza quando ti riconnetti.

windows.ui.thisjournallinkisntvalid = questo link del diario non è valido.

windows.ui.thislinkcouldnotbedismissedtry = non è stato possibile ignorare questo link. riprova.

windows.ui.thislinkcouldnotbeusedtry = non è stato possibile usare questo link. riprova.

windows.ui.thislistappliestoactivitytoo = questo elenco vale anche per l'attività

windows.ui.thisonlytakesamoment = questo richiede solo un momento

windows.ui.thispc = questo PC

windows.ui.thispccantconnecttoyour = questo PC non può connettersi al tuo diario con l'accoppiamento salvato. ottieni un nuovo link di accoppiamento dal tuo diario e accoppia di nuovo.

windows.ui.thispccouldntworkoutyour = questo PC non ha potuto ricavare la marca del tuo diario, quindi non c'è niente da confrontare. continua solo se sai che il link viene dal tuo diario.

windows.ui.thisreadsthewindowtitleandmatches = questo legge il titolo della finestra e fa corrispondere le finestre private in Edge e Firefox. il controllo è stato fatto su Edge 153 e Firefox 156, entrambi impostati sull'inglese; un aggiornamento del browser o un'altra lingua può cambiare un titolo, e allora una finestra privata lì può arrivare nel tuo diario. Chrome 153 e Brave 153 non mostrano la modalità privata nel titolo, e gli altri browser non vengono riconosciuti, quindi le loro finestre private arrivano nel tuo diario. per tenere ogni finestra di un browser fuori dal tuo diario, aggiungi quel browser alle app escluse sotto.

windows.ui.thisremovesthecopyonthisdevicesoitwontreachyourj = questo rimuove la copia su questo dispositivo, quindi non arriverà nel tuo diario.

windows.ui.thisremovesthecopyonthisdeviceyourjournalmaystil = questo rimuove la copia su questo dispositivo. il tuo diario può ancora portare la copia che ha già.

windows.ui.thissetuplinkcouldnotbeused = non è stato possibile usare questo link di configurazione iniziale. torna al tuo diario per un nuovo link, oppure incolla qui un link di accoppiamento.

windows.ui.thisstepcouldnotfinishtryagain = non è stato possibile finire questo passaggio. riprova.

windows.ui.thisstepcouldntfinishtryagain = non è stato possibile finire questo passaggio. riprova.

windows.ui.titlekeywords = parole chiave del titolo

windows.ui.titlekeywordtoexclude = parola chiave del titolo da escludere

windows.ui.toolargebring = troppo grande per portarlo

windows.ui.trynow = prova ora

windows.ui.trynowarg0 = prova ora, { $arg0 }

windows.ui.tryopeningthelinkagain = prova ad aprire di nuovo il link.

windows.ui.turningoffstopsbringingchangeswhat = disattivare questo smette di portare le modifiche. ciò che è già nel tuo diario resta lì, e ciò che è in attesa resta su questo computer finché non lo attivi di nuovo.

windows.ui.turnitonanytime = attivalo in qualsiasi momento.

windows.ui.turnoff = disattiva

windows.ui.turnon = attiva { $accessiblename }

windows.ui.unavailable = non disponibile

windows.ui.unavailableword = non disponibile

windows.ui.unknown = sconosciuto

windows.ui.unknownjournals = diari sconosciuti

windows.ui.untiliresume = finché non riprendo

windows.ui.usethislink = usa questo link

windows.ui.usevault = usa questo vault

windows.ui.value15minutes = 15 minuti

windows.ui.value1hour = 1 ora

windows.ui.value30minutes = 30 minuti

windows.ui.vaults = vault

windows.ui.vaultscomputer = vault su questo computer

windows.ui.vaultwithnamealreadyjournal = un vault con questo nome è già nel tuo diario

windows.ui.viewdetails = vedi i dettagli

windows.ui.waiting = in attesa

windows.ui.waitingforajournalupdateyourscreenshotswaitonthi = in attesa di un aggiornamento del diario. i tuoi screenshot aspettano su questo PC.

windows.ui.waitingforyouranswer = in attesa della tua risposta

windows.ui.waitingforyoutoconfirmyourjournalsmark = in attesa che tu confermi la marca del tuo diario

windows.ui.waitingjournalupdate = in attesa di un aggiornamento del diario

windows.ui.waitingreachjournal = in attesa di connettersi al tuo diario

windows.ui.waitingsync = { $value } in attesa di sincronizzazione

windows.ui.waitingtogotoyourjournal = in attesa di andare nel tuo diario

windows.ui.waitingtogotoyourjournalx5d789db5 = in attesa di andare nel tuo diario.

windows.ui.waitingtosync = in attesa di sincronizzazione

windows.ui.waitingword = in attesa

windows.ui.whatanswered = che cosa ha risposto

windows.ui.whatsalreadyinyourjournalstayswhenyoupauseorturn = ciò che è già nel tuo diario resta quando metti in pausa o disattivi questo.

windows.ui.whatyousharestaysonthispc = ciò che condividi resta su questo PC finché non va nel tuo diario sui tuoi dispositivi. il tuo diario è sempre privato, solo tuo.

windows.ui.whatyouturnongoesintoyour = ciò che attivi va nel tuo diario.

windows.ui.whilethisisonthesolstoneapptakesinsoundandallofi = mentre questo è attivo, l'app di solstone prende l'audio, e tutto va nel tuo diario.

windows.ui.whilethisisonthesolstoneapptakesinthescreenshots = mentre questo è attivo, l'app di solstone prende gli screenshot che fai su questo PC, e il tuo diario tiene ognuno al minuto in cui l'hai fatto.

windows.ui.whilethisisonthesolstoneapptakesinwhatsonyourscr = mentre questo è attivo, l'app di solstone prende ciò che c'è sul tuo schermo, e tutto va nel tuo diario.

windows.ui.windowsfromtheseappsneverreachyour = le finestre di queste app non arrivano mai nel tuo diario.

windows.ui.windowswhosetitlecontainsawordyou = le finestre il cui titolo contiene una parola che scegli non arrivano mai nel tuo diario.

windows.ui.withactivity = con attività

windows.ui.withcomputersound = con audio del computer

windows.ui.yesterday = ieri

windows.ui.youcanturneachsourceonor = puoi attivare o disattivare ogni fonte in fonti.

windows.ui.youchoosewhichsourcesareon = scegli tu quali fonti sono attive.

windows.ui.youllseehowmanyscreenshotstherearebeforeanycomei = vedrai quanti screenshot ci sono prima che ne entri uno.

windows.ui.your = tuo

windows.ui.yourjournal = tuo · diario

windows.ui.yourjournalcanttakethiskindoffile = il tuo diario non può prendere questo tipo di file

windows.ui.yourjournalcouldntacceptthis = il tuo diario non ha potuto accettare questo.

windows.ui.yourjournaldoesntsupportthismove = il tuo diario non supporta ancora questa operazione. tutto ciò che è in attesa di invio resta su questo PC. aggiorna il tuo diario, poi riprova.

windows.ui.yourjournalneedsanewlink = il tuo diario ha bisogno di un nuovo link

windows.ui.yourjournalnotsetupyet = il tuo diario, non ancora configurato

windows.ui.yourjournalopenedbutsetupprogresscould = il tuo diario si è aperto, ma non è stato possibile salvare l'avanzamento della configurazione iniziale. riprova.

windows.ui.yourjournalshowsthissamemarkbeside = il tuo diario mostra questa stessa marca accanto al link di accoppiamento. deve corrispondere, esattamente.

windows.ui.yourjournalsmarkunavailablerightnow = la marca del tuo diario, non disponibile in questo momento

windows.ui.yourjournalstoppedacceptingthisdeviceimportswait = il tuo diario ha smesso di accettare questo dispositivo. le importazioni aspettano qui finché non accoppi di nuovo.

windows.ui.yourshortcut = la tua scorciatoia

windows.ui.yousaidthismarkdoesntmatch = hai detto che questa marca non corrisponde a quella che il tuo diario mostra, quindi questo PC non è accoppiato. potresti aver incollato il link sbagliato, o qualcosa non va. incolla un link nuovo dal tuo diario, oppure scrivi a support@solstone.app e ti aiutiamo.
