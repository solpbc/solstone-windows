windows.about.app = app de windows

windows.about.dayago = { $count ->
    [one] hace { $count } día
    [many] hace { $count } días
   *[other] hace { $count } días
}

windows.about.hourago = { $count ->
    [one] hace { $count } hora
    [many] hace { $count } horas
   *[other] hace { $count } horas
}

windows.about.journal = diario

windows.about.justnow = justo ahora

windows.about.lastseen = { $line } · visto por última vez { $age }

windows.about.line0 = { $label } { $version }

windows.about.line1 = { $label } { $version } ({ $build })

windows.about.line2 = { $label } { $version } · { $platform }

windows.about.line3 = { $label } { $version } ({ $build }) · { $platform }

windows.about.line4 = { $label } { $version } · { $arch }

windows.about.line5 = { $label } { $version } ({ $build }) · { $arch }

windows.about.line6 = { $label } { $version } · { $platform } · { $arch }

windows.about.line7 = { $label } { $version } ({ $build }) · { $platform } · { $arch }

windows.about.minuteago = { $count ->
    [one] hace { $count } minuto
    [many] hace { $count } minutos
   *[other] hace { $count } minutos
}

windows.about.unknown = diario desconocido

windows.browser.discarded = se descartó

windows.desktop.shared.about.journalunknown = diario desconocido

windows.desktop.shared.about.opensourcelocalfirst = código abierto, local primero.

windows.desktop.shared.about.pendingsettingstab = diario

windows.desktop.shared.about.setupwelcomebody1 = la app de solstone recibe lo que compartes con ella, y todo va a tu diario.

windows.desktop.shared.about.textcontent = copiar

windows.desktop.shared.about.textcontent2 = no se pudo copiar. selecciona el texto y cópialo.

windows.desktop.shared.app.aboutsolstone = acerca de solstone

windows.desktop.shared.app.emptytext = todavía no hay nada

windows.desktop.shared.app.frontendorigin = configuración

windows.desktop.shared.app.justnow = justo ahora

windows.desktop.shared.app.microphonemodeoff = desactivado

windows.desktop.shared.app.notpaired = sin vincular

windows.desktop.shared.app.offlinetitle = no se puede conectar con tu diario

windows.desktop.shared.app.settingsdiagnosticson = activado

windows.desktop.shared.app.sourcestitle = fuentes

windows.desktop.shared.copy.audiosourcesystem = audio del sistema

windows.desktop.shared.copy.checkagain = revisar de nuevo

windows.desktop.shared.copy.choosedevice = elegir un dispositivo

windows.desktop.shared.copy.decidingtitle = guardando tu elección

windows.desktop.shared.copy.decisionrefusedbody = tu diario no pudo aplicar esta elección. tu conexión actual sigue funcionando.

windows.desktop.shared.copy.decisionrefusedtitle = la elección necesita atención

windows.desktop.shared.copy.decisionunknownbody = tu diario todavía no confirmó el resultado.

windows.desktop.shared.copy.decisionunknowntitle = revisando tu elección

windows.desktop.shared.copy.deferchoice = ahora no

windows.desktop.shared.copy.journalmarkconfirmbutton = sí, este es mi diario

windows.desktop.shared.copy.journalmarkconfirmquestion = ¿esto coincide con tu diario?

windows.desktop.shared.copy.journalmarkmismatchbutton = eso no coincide

windows.desktop.shared.copy.journalmarkunverifiedcancelbutton = cancelar la vinculación

windows.desktop.shared.copy.journalmarkunverifiedcontinuebutton = continuar de todos modos

windows.desktop.shared.copy.journalmarkunverifiedtitle = no se pudo verificar

windows.desktop.shared.copy.keepboth = conservar ambos

windows.desktop.shared.copy.listunavailablebody = no se pudieron cargar los dispositivos en tu diario. vuelve a intentarlo cuando se pueda conectar con tu diario.

windows.desktop.shared.copy.listunavailabletitle = dispositivos no disponibles

windows.desktop.shared.copy.offlinebody = lo que está en espera para enviarse se queda en esta PC. vuelve a intentarlo cuando se pueda conectar con tu diario.

windows.desktop.shared.copy.pickerempty = no hay otros dispositivos vinculados

windows.desktop.shared.copy.replaceconfirmbody = esta PC continúa con su nombre y su historial. el dispositivo elegido perderá el acceso a tu diario.

windows.desktop.shared.copy.replaceconfirmtitle = ¿reemplazar "{ $selecteddevicelabel }"?

windows.desktop.shared.copy.replaceconfirmtitlefallback = ¿reemplazar el dispositivo elegido?

windows.desktop.shared.copy.replacedevice = reemplazar el dispositivo

windows.desktop.shared.copy.replaceofferbody = puedes conservar ambos, o elegir un dispositivo para que esta PC lo reemplace.

windows.desktop.shared.copy.replaceoffertitle = ¿esto reemplaza uno de tus dispositivos?

windows.desktop.shared.copy.settingsjournalopen = abrir el diario

windows.desktop.shared.copy.settingslogexportsave = guardar una copia…

windows.desktop.shared.copy.settingslogexporttitle = registros recientes

windows.desktop.shared.copy.settingstabattentiona11y = necesita atención

windows.desktop.shared.copy.settingstabupdatesdonea11y = solstone está al día

windows.desktop.shared.copy.setupapplabel = la app de solstone

windows.desktop.shared.copy.setupback = atrás

windows.desktop.shared.copy.setupconnectedtoyourjournal = conectado a tu diario

windows.desktop.shared.copy.setupcontinue = continuar

windows.desktop.shared.copy.setupjournalcardheading = tu diario

windows.desktop.shared.copy.setupmicrophoneheadline = comparte tus conversaciones

windows.desktop.shared.copy.setupopenyourjournal = abrir tu diario

windows.desktop.shared.copy.setupscreenframecaption = tu pantalla, hace un momento

windows.desktop.shared.copy.setupwelcomebody2 = tu diario es siempre privado, solo tuyo.

windows.desktop.shared.copy.setupwindowtitle = configurar solstone

windows.desktop.shared.copy.sourcesbrowserconnectednow = conectado ahora

windows.desktop.shared.copy.sourcesbrowserdiscard = descartar las páginas del navegador en espera

windows.desktop.shared.copy.sourcesbrowserdiscardcommit = descartar páginas

windows.desktop.shared.copy.sourcesbrowserdiscardconfirm = las páginas del navegador que todavía están en espera para enviarse se quitarán de esta PC.

windows.desktop.shared.copy.sourcesbrowserheld = ahora no se están recibiendo páginas nuevas del navegador.

windows.desktop.shared.copy.sourcesbrowserpages = páginas del navegador

windows.desktop.shared.copy.storageunavailablebody = esta PC no pudo leer su conexión guardada. lo que está en espera para enviarse no se quitó.

windows.desktop.shared.copy.storageunavailabletitle = conexión guardada no disponible

windows.desktop.shared.copy.targetmissingbody = ese dispositivo ya no está en la lista de tu diario. elige otro dispositivo, o conserva ambos.

windows.desktop.shared.copy.targetmissingtitle = el dispositivo ya no está disponible

windows.desktop.shared.copy.technicaldetails = detalles técnicos

windows.desktop.shared.copy.text = { $value } · { $value2 }

windows.desktop.shared.copy.tryagain = reintentar

windows.desktop.shared.settings.add = agregar

windows.desktop.shared.settings.excludedapps = apps excluidas

windows.desktop.shared.settings.gethelp = obtener ayuda

windows.desktop.shared.settings.help = ayuda

windows.desktop.shared.settings.reportaproblem = informar un problema

windows.desktop.shared.settings.severity = aceptar

windows.desktop.shared.settings.textcontent = reintentar

windows.desktop.shared.settings.updates = actualizaciones

windows.desktop.shared.settings.windowtitles = títulos de ventana

windows.desktop.shared.setup.cancel = cancelar

windows.desktop.shared.setup.pair = vincular

windows.desktop.shared.setup.startatlogin = iniciar al iniciar sesión

windows.desktop.shared.setup.wizardpausefailure = pausar

windows.desktop.shared.setup.wizardpausefailure2 = reanudar

windows.desktop.shared.updates.automaticupdates = actualizaciones automáticas

windows.desktop.shared.updates.checkforupdatesautomatically = buscar actualizaciones automáticamente

windows.desktop.shared.updates.checknow = buscar ahora

windows.desktop.shared.updates.dismiss = ocultar

windows.desktop.shared.updates.download = descargar

windows.desktop.shared.updates.downloadupdatesinthebackground = descargar actualizaciones en segundo plano

windows.desktop.shared.updates.everyday = todos los días

windows.desktop.shared.updates.everymonth = todos los meses

windows.desktop.shared.updates.everyweek = todas las semanas

windows.desktop.shared.updates.howoften = con qué frecuencia

windows.desktop.shared.updates.nevercheckedforupdates = nunca se buscaron actualizaciones

windows.desktop.shared.updates.textcontent = leer las notas completas en línea

windows.desktop.shared.updates.whatsnew = novedades

windows.exclusions.activity = { $count ->
    [one] { $display } fotograma dejado fuera de tu diario en esta sesión
    [many] { $display } fotogramas dejados fuera de tu diario en esta sesión
   *[other] { $display } fotogramas dejados fuera de tu diario en esta sesión
}

windows.exclusions.activityredacted = { $count ->
    [one] { $display } fotograma dejado fuera de tu diario en esta sesión · { $more } más llegaron a tu diario con una ventana excluida en negro
    [many] { $display } fotogramas dejados fuera de tu diario en esta sesión · { $more } más llegaron a tu diario con una ventana excluida en negro
   *[other] { $display } fotogramas dejados fuera de tu diario en esta sesión · { $more } más llegaron a tu diario con una ventana excluida en negro
}

windows.health.attentioncount = { $count ->
    [one] { $display } necesita atención
    [many] { $display } necesitan atención
   *[other] { $display } necesitan atención
}

windows.import.allhistory = todo el historial de importación en tu diario

windows.import.expiredword = vencida

windows.import.holdoffline = en espera de que tu diario vuelva a conectarse.

windows.import.intro = agrega cualquier archivo que elijas aquí o sueltes en esta ventana. tu diario averigua qué es.

windows.import.morehelper = guías, opciones y el historial de importación en tu diario

windows.import.readytoreviewword = pendiente de revisión

windows.import.recenta11y = { $name }, en tu diario, { $relative }. abrir en tu diario.

windows.import.recentmeta = en tu diario · { $relative }

windows.import.removereadybody = esto quita la copia en este dispositivo. la copia en espera en tu diario se queda ahí hasta que la descartes o venza.

windows.import.reviewaction = revisar en tu diario

windows.import.reviewactiona11y = revisar { $name } en tu diario

windows.import.reviewdetail = se guardó en tu diario. previsualiza y confirma para empezar.

windows.import.reviewdetailbody = se guardó en tu diario. revísalo en menos de una hora, antes de que tu diario quite la copia en espera.

windows.import.reviewexpired = la copia en espera en tu diario venció. este dispositivo todavía tiene el archivo.

windows.import.rowsending = enviando a tu diario…

windows.import.sectionreview = en espera de tu revisión

windows.import.sendagainaction = enviar de nuevo para revisar

windows.import.sendagainactiona11y = enviar { $name } de nuevo para revisar

windows.import.statusattentionheadline = las importaciones te necesitan

windows.import.statusattentionsub = abre la importación para ver por qué.

windows.import.statusreviewheadline = { $count ->
    [one] { $display } importación lista para revisar
    [many] { $display } importaciones listas para revisar
   *[other] { $display } importaciones listas para revisar
}

windows.import.statusreviewsub = abre la importación para revisar y empezar.

windows.import.tilea11y = importación, { $state }. { $sub }.

windows.import.tilereadycount = { $display } para revisar en tu diario

windows.import.tilesendingcount = { $count ->
    [one] { $display } archivo
    [many] { $display } archivos
   *[other] { $display } archivos
}

windows.import.tilewaitingcount = { $display } en espera

windows.import.tilewaitingjournal = en espera de tu diario

windows.logs.availablelogsareshownbelowcheckthe = los registros disponibles se muestran abajo. revisa las notas en busca de huecos antes de guardar una copia.

windows.logs.copysaved = copia guardada.

windows.logs.couldntreadthelogfilestry = no se pudieron leer los archivos de registro. vuelve a intentarlo.

windows.logs.nologfilesareavailableyettry = todavía no hay archivos de registro. vuelve a intentarlo después de usar la app.

windows.logs.norecentlogsareavailableyettry = todavía no hay registros recientes. vuelve a intentarlo después de usar la app.

windows.logs.readthelogsbelowthenchoosewhether = lee los registros de abajo y luego elige si guardar una copia.

windows.logs.textcontent = lee los registros recientes de la app de solstone antes de guardar una copia para un informe de soporte. no se envía nada desde aquí.

windows.logs.textcontent2 = leer registros recientes

windows.logs.textcontent3 = leyendo registros…

windows.logs.textcontent4 = no se pudieron leer los registros. vuelve a intentarlo.

windows.logs.textcontent5 = no se pudo guardar la copia. intenta guardarla en otra carpeta.

windows.notice.everythingsavedonthispcisgoing = todo lo guardado en esta PC está yendo a tu diario ahora.

windows.notice.everythingyouturnedonisgoinginto = todo lo que activaste está yendo a tu diario de nuevo.

windows.notice.intakeisrunningagain = la recepción está funcionando de nuevo

windows.notice.intakeranintoaproblem = la recepción tuvo un problema

windows.notice.itsgoingintoyourjournalagain = esto está yendo a tu diario de nuevo.

windows.notice.solstoneisstilloneverythingissaved = solstone sigue activado. todo está guardado en esta PC y va a tu diario cuando el diario vuelva a estar al alcance.

windows.notice.theisback = { $value } volvió

windows.notice.theneedsyourpermission = { $value } necesita tu permiso

windows.notice.thesolstoneiconinthetaskbarsays = el icono de solstone en la barra de tareas dice qué está mal.

windows.notice.windowsprivacysettingsareblockingitturn = la configuración de privacidad de windows bloquea esto. activa el acceso a { $value } para las apps de escritorio, y esto vuelve a ir a tu diario.

windows.notice.yourjournalisbackinreach = tu diario volvió a estar al alcance

windows.notice.yourjournalisoutofreach = tu diario está fuera de alcance

windows.obsidian.chooseobsidianvault = elige tu bóveda de Obsidian

windows.obsidian.vault = bóveda

windows.obsidian.warning = tu diario ya tiene una bóveda llamada { $vault } de otro dispositivo. si continúas, los cambios de ambos dispositivos van a tu diario.

windows.obsidian.warningdate = tu diario ya tiene una bóveda llamada { $vault } de otro dispositivo, actualizada por última vez el { $date }. si continúas, los cambios de ambos dispositivos van a tu diario.

windows.obsidian.warningdevice = tu diario ya tiene una bóveda llamada { $vault } de { $device }. si continúas, los cambios de ambos dispositivos van a tu diario.

windows.obsidian.warningdevicedate = tu diario ya tiene una bóveda llamada { $vault } de { $device }, actualizada por última vez el { $date }. si continúas, los cambios de ambos dispositivos van a tu diario.

windows.pill.held = confirmar la marca

windows.pill.heldwaiting = confirmar la marca · { $waiting } en espera

windows.pill.offline = sin conexión

windows.pill.offlinewaiting = sin conexión · { $waiting } en espera

windows.pill.readytoreview = { $display } para revisar

windows.pill.refused = vincular de nuevo

windows.pill.refusedwaiting = vincular de nuevo · { $waiting } en espera

windows.pill.syncpaused = sincronización en pausa

windows.pill.syncpausedwaiting = sincronización en pausa · { $waiting } en espera

windows.quantity.hours = { $count ->
    [one] { $display } hora
    [many] { $display } horas
   *[other] { $display } horas
}

windows.quantity.items = { $count ->
    [one] { $display } elemento
    [many] { $display } elementos
   *[other] { $display } elementos
}

windows.quantity.minutes = { $count ->
    [one] { $display } minuto
    [many] { $display } minutos
   *[other] { $display } minutos
}

windows.quantity.notes = { $count ->
    [one] { $display } nota
    [many] { $display } notas
   *[other] { $display } notas
}

windows.quantity.pages = { $count ->
    [one] { $display } página
    [many] { $display } páginas
   *[other] { $display } páginas
}

windows.quantity.screenshots = { $count ->
    [one] { $display } captura de pantalla
    [many] { $display } capturas de pantalla
   *[other] { $display } capturas de pantalla
}

windows.screenshots.bringcount = { $count ->
    [one] traer { $display } captura de pantalla
    [many] traer { $display } capturas de pantalla
   *[other] traer { $display } capturas de pantalla
}

windows.screenshots.cantcount = { $count ->
    [one] { $display } captura de pantalla no puede entrar, así que se queda en la carpeta.
    [many] { $display } capturas de pantalla no pueden entrar, así que se quedan en la carpeta.
   *[other] { $display } capturas de pantalla no pueden entrar, así que se quedan en la carpeta.
}

windows.screenshots.unreadablecount = { $count ->
    [one] no se pudo leer { $display } captura de pantalla.
    [many] no se pudieron leer { $display } capturas de pantalla.
   *[other] no se pudieron leer { $display } capturas de pantalla.
}

windows.sharedsources.also = también en

windows.sharedsources.labelage = { $label } · { $relative }

windows.sharedsources.labelstate = { $label } · { $state }

windows.sharedsources.labelstateage = { $label } · { $state } · { $relative }

windows.sharedsources.many = de otros { $count } dispositivos

windows.sharedsources.one = de otro dispositivo

windows.sharedsources.stateage = { $state } · { $relative }

windows.time.checkedday = { $count ->
    [one] se revisó hace { $display } día
    [many] se revisó hace { $display } días
   *[other] se revisó hace { $display } días
}

windows.time.checkedhour = { $count ->
    [one] se revisó hace { $display } hora
    [many] se revisó hace { $display } horas
   *[other] se revisó hace { $display } horas
}

windows.time.checkedminute = { $count ->
    [one] se revisó hace { $display } minuto
    [many] se revisó hace { $display } minutos
   *[other] se revisó hace { $display } minutos
}

windows.time.nextsave = en { $minutes }:{ $seconds }

windows.time.takenhours = { $count ->
    [one] hace { $display } hora
    [many] hace { $display } horas
   *[other] hace { $display } horas
}

windows.tray.checkforupdates = buscar actualizaciones…

windows.tray.hrmin = { $count } h { $count2 } min

windows.tray.lessthanaminute = menos de un minuto

windows.tray.needsarestart = necesita un reinicio

windows.tray.onbut = activado, pero { $needs }

windows.tray.onconnectedtoyourjournal = activado, conectado a tu diario

windows.tray.onnotconnectedtoajournal = activado, no conectado a un diario

windows.tray.onsavedonthispc = activado, guardado en esta PC

windows.tray.pausedleft = en pausa, quedan { $value }

windows.tray.solstonewaitingforyoutoconfirmyour = solstone · esperando a que confirmes la marca de tu diario

windows.tray.starting = iniciando…

windows.tray.storagefaultforowner = no se puede guardar en esta PC

windows.tray.text = { $tooltip } { $notice }

windows.tray.theneedspermissioninwindowssettings = { $value } necesita permiso en la configuración de windows

windows.tray.thestoppedgoingintoyourjournal = { $value } dejó de ir a tu diario

windows.tray.unknownjournalseenat = diario desconocido visto en { $name }

windows.tray.unknownjournalseenthroughtherelay = diario desconocido visto a través del servidor de retransmisión

windows.tray.updateavailable = actualización disponible

windows.tray.updatecheckfailed = falló la búsqueda de actualizaciones

windows.tray.updateready = actualización lista · se instala al salir y volver a abrir

windows.ui.accesswasdenied = se denegó el acceso.

windows.ui.accesswasdeniedcheckwindowspermissionsfor = se denegó el acceso. revisa los permisos de windows para esta fuente y vuelve a intentarlo.

windows.ui.activex474d1b75 = activo:

windows.ui.activex68f50a9b = { $leading }· activo

windows.ui.activex96879611 = activo

windows.ui.activity = actividad

windows.ui.activityisstillon = la actividad sigue activada.

windows.ui.addappnameswindowtitlesandpage = agrega a tu diario los nombres de las apps, los títulos de ventana y las direcciones de página, y si estás en actividad o en inactividad, o si esta PC está bloqueada. está activado mientras la pantalla está encendida, y se pausa con ella. las direcciones de página incluyen el sitio y la ruta, sin nada después de ? o #.

windows.ui.addmore = agregar más

windows.ui.addspdfsaudioimagesandtextyouchoosehereordropont = elige archivos para traerlos a tu diario, o suéltalos en esta ventana.

windows.ui.addthesolstoneextension = agrega la extensión de solstone.

windows.ui.addto = agregar a { $value }

windows.ui.aglobalshortcuttopauseandresume = un atajo global para pausar y reanudar la recepción desde cualquier lugar.

windows.ui.allcaughtup = todo al día

windows.ui.allofit = todo

windows.ui.allsourceson = todas las fuentes activadas

windows.ui.alouderinputforquietmicrophoneschanges = una entrada más alta para micrófonos con poco volumen. los cambios se aplican enseguida. una ganancia más fuerte puede recoger más ruido de fondo en una habitación silenciosa.

windows.ui.alreadyonhome = ya en inicio

windows.ui.anewlinkfromyourjournalcanbepastedhere = aquí se puede pegar un enlace nuevo de tu diario.

windows.ui.anotherpairinglinkcouldnotbeadded = no se pudo agregar otro enlace de vinculación.

windows.ui.april = abril

windows.ui.arg0 = { $leading }· { $arg0 }

windows.ui.arg0arg1 = { $arg0 }
    { $arg1 }

windows.ui.arg0arg1arg2 = { $arg1 } de { $arg0 } de { $arg2 }

windows.ui.arg0bytes = { $arg0 } bytes

windows.ui.arg0gb = { $arg0 } GB

windows.ui.arg0hr = { $arg0 } h

windows.ui.arg0hrleft = { $arg0 } h restantes

windows.ui.arg0kb = { $arg0 } KB

windows.ui.arg0kbx75b11ba1 = { $arg0 } KB

windows.ui.arg0mb = { $arg0 } MB

windows.ui.arg0mbxff882f00 = { $arg0 } MB

windows.ui.arg0min = { $arg0 } min

windows.ui.arg0minago = hace { $arg0 } min

windows.ui.arg0minleft = { $arg0 } min restantes

windows.ui.arg0needsattention = { $arg0 } necesita atención

windows.ui.arg0ofarg1inyourjournal = { $arg0 } de { $arg1 } en tu diario

windows.ui.arg0ofarg1screenshotsinyourjournal = { $arg0 } de { $arg1 } capturas de pantalla en tu diario

windows.ui.arg0tb = { $arg0 } TB

windows.ui.arg0x211b8ba1 = { $arg0 }%

windows.ui.ascreenshotalreadyinyourjournalstaysthereifyoude = una captura de pantalla que ya está en tu diario se queda ahí si la eliminas de la carpeta.

windows.ui.ascreenshotthatsonlycopiedtotheclipboardisntsave = una captura de pantalla que solo se copia al portapapeles no se guarda, así que no entra.

windows.ui.asegmentisa5minutelocal = un segmento es un paquete local de 5 minutos que se queda aquí hasta que tu diario lo recibe.

windows.ui.attentionneeded = se necesita atención

windows.ui.audio = audio

windows.ui.august = agosto

windows.ui.automaticchecksareoff = las búsquedas automáticas de actualizaciones están desactivadas

windows.ui.automaticchecksareon = las búsquedas automáticas de actualizaciones están activadas

windows.ui.awindowthatclosesormovescan = una ventana que se cierra o se mueve puede aparecer hasta por un fotograma antes de que se aplique la exclusión.

windows.ui.backgrounddatauserestricted = el uso de datos en segundo plano está restringido

windows.ui.browsers = navegadores

windows.ui.browserstateunavailable = estado del navegador no disponible

windows.ui.change = cambiar

windows.ui.checkedjustnow = se buscó justo ahora

windows.ui.checkforupdates = buscar actualizaciones

windows.ui.checking = revisando…

windows.ui.checkingforupdates = buscando actualizaciones…

windows.ui.checkingword = revisando…

windows.ui.checkingyourjournal = revisando tu diario.

windows.ui.chooseafolder = elige una carpeta.

windows.ui.chooseafolderxdbec742d = elegir una carpeta…

windows.ui.chooseanapptoexclude = elige una app para excluir

windows.ui.choosearunningapp = elegir una app en ejecución…

windows.ui.choosefiles = elegir archivos…

windows.ui.choosefolder = elegir una carpeta

windows.ui.choosehowfarbacktostartthenbringthemin = elige desde cuándo empezar y luego recíbelas.

windows.ui.choosethefolderyourscreenshotssaveto = elige la carpeta donde se guardan tus capturas de pantalla

windows.ui.choosewhatstaysoutofyourjournal = elige qué se queda fuera de tu diario. los cambios se aplican enseguida.

windows.ui.claimednotverified = declarada, no verificada

windows.ui.clickthenpressyourshortcut = haz clic y luego presiona tu atajo

windows.ui.closenavigation = cerrar la navegación

windows.ui.computersound = sonido de la computadora

windows.ui.computersoundlevel = nivel del sonido de la computadora

windows.ui.confirmthemark = confirmar la marca

windows.ui.confirmthemarkarg0waiting = confirmar la marca · { $arg0 } en espera

windows.ui.confirmyourjournalsmarktoopen = confirma la marca de tu diario para abrirlo

windows.ui.connectajournal = conectar un diario

windows.ui.connected = conectado

windows.ui.connectedto = conectado a { $value }

windows.ui.connecting = conectando…

windows.ui.connectingx3d6df245 = conectando

windows.ui.connection = conexión

windows.ui.connecttoyourjournal = conectar con tu diario

windows.ui.connectyourjournal = conecta tu diario

windows.ui.continuethisconnection = continuar esta conexión

windows.ui.copied = copiado

windows.ui.copythefulllinkfromyourchatandopenitagain = copia el enlace completo de tu chat y ábrelo de nuevo.

windows.ui.couldnotpausetryagain = no se pudo pausar. vuelve a intentarlo.

windows.ui.couldnotresumetryagain = no se pudo reanudar. vuelve a intentarlo.

windows.ui.couldnotsavethischoiceitmay = no se pudo guardar esta elección. puede cambiar después de un reinicio. vuelve a intentarlo.

windows.ui.couldnotsavethisdismissaltryagain = no se pudo guardar al ocultar esto. vuelve a intentarlo.

windows.ui.couldnotsaveturningthisoffitisoffnowbutmayturnon = no se pudo guardar al desactivar esto. está desactivado ahora, pero puede volver a activarse después de un reinicio. vuelve a intentarlo.

windows.ui.couldnotsaveturningthisonitstaysofftryagain = no se pudo guardar al activar esto. se queda desactivado. vuelve a intentarlo.

windows.ui.couldnotsaveyoursetupprogresstry = no se pudo guardar el avance de la configuración inicial. vuelve a intentarlo.

windows.ui.couldnotupdatestartatlogin = no se pudo actualizar el inicio al iniciar sesión

windows.ui.couldntcheckforupdates = no se pudo buscar actualizaciones

windows.ui.couldntcheckforupdatessolstonewas = no se pudo buscar actualizaciones. solstone { $v } se encontró antes

windows.ui.couldntcountyourscreenshots = no se pudieron contar tus capturas de pantalla.

windows.ui.couldntdownloadsolstone = no se pudo descargar solstone { $value }

windows.ui.couldntdownloadupdate = no se pudo descargar la actualización

windows.ui.couldntfindascreenshotsfolderonthispcchoosethefo = no se pudo encontrar la carpeta de capturas de pantalla en esta PC. elige la carpeta donde se guardan tus capturas de pantalla.

windows.ui.couldntfinishdiscardingwhatsleft = no se pudo terminar de descartar. lo que queda sigue en esta PC.

windows.ui.couldntinstallupdate = no se pudo instalar la actualización

windows.ui.couldntloadthesolstoneapps = no se pudo cargar el estado de la app de solstone justo ahora.

windows.ui.couldntopenthestorepagein = no se pudo abrir la página de la tienda en { $value }.

windows.ui.couldntopenyourjournal = no se pudo abrir tu diario.

windows.ui.couldntopenyourjournaltryagain = no se pudo abrir tu diario. vuelve a intentarlo.

windows.ui.couldntreachyourjournaloveryour = no se pudo conectar con tu diario por tu red privada. únete a la misma wi-fi que tu diario y luego vuelve a intentarlo con un enlace de vinculación nuevo de tu diario.

windows.ui.couldntreadimportsonthisdevicenothinghasbeenremo = no se pudieron leer las importaciones en este dispositivo. no se quitó nada. reinicia la app de solstone para volver a intentarlo.

windows.ui.couldntreadyoursavedsourcechoices = no se pudieron leer tus elecciones de fuentes guardadas. las fuentes están desactivadas. elige tus fuentes de nuevo en configuración.

windows.ui.couldntregisterthatshortcut = no se pudo registrar ese atajo

windows.ui.couldntremovethistryagain = no se pudo quitar esto. vuelve a intentarlo.

windows.ui.couldntturntryagain = no se pudo activar esto. vuelve a intentarlo.

windows.ui.couldntverifyjournalsonothing = no se pudo verificar tu diario, así que no se conectó nada. vuelve a intentarlo, o pega un enlace de vinculación.

windows.ui.dayago = { $count ->
    [one] hace { $count } día
    [many] hace { $count } días
   *[other] hace { $count } días
}

windows.ui.december = diciembre

windows.ui.delivered = { $value } entregados

windows.ui.deliveredpending = { $value } entregados · { $value2 } pendientes

windows.ui.desktopactivity = actividad de escritorio

windows.ui.devicechoice = elección de dispositivo

windows.ui.devicepriority = prioridad del dispositivo

windows.ui.disable = desactivar

windows.ui.dontstartatlogin = no iniciar al iniciar sesión

windows.ui.downloaddeferredconnectionstatusunknown = descarga diferida: se desconoce el estado de la conexión

windows.ui.downloaddeferredmeteredconnection = descarga diferida en una conexión de uso medido

windows.ui.downloaddeferredwhileroaming = descarga diferida durante la itinerancia

windows.ui.downloadingsolstone = descargando solstone { $v }

windows.ui.downloadthelatestfromsolstoneappdownloadwindows = descarga lo último desde solstone.app/download/windows

windows.ui.edgeandfirefoxsettoenglish = Edge y Firefox configurados en inglés

windows.ui.enable = activar

windows.ui.enablethepauseresumeshortcut = activar el atajo de pausar / reanudar

windows.ui.errorword = error

windows.ui.esctocancel = esc para cancelar

windows.ui.everypicturesavedinarg0comesinnotonlyyourscreens = cada imagen guardada en { $arg0 } entra, no solo tus capturas de pantalla.

windows.ui.everythingisinyourjournal = todo está en tu diario

windows.ui.exclude = excluir

windows.ui.exclusionactivity = actividad de exclusión

windows.ui.failedtoacquiresetuplock = no se pudo obtener el bloqueo de la configuración inicial

windows.ui.february = febrero

windows.ui.file = archivo

windows.ui.files = archivos

windows.ui.finishconnecting = terminar de conectar

windows.ui.finishconnectingtoyourjournal = terminar de conectar con tu diario

windows.ui.finishthecurrentconnectionthentryagain = termina la conexión actual y luego vuelve a intentarlo.

windows.ui.folder = carpeta

windows.ui.foundjournalpc = se encontró tu diario en esta PC

windows.ui.from = de { $value }

windows.ui.frombeforetoday = de antes de hoy

windows.ui.gettingready = preparando…

windows.ui.gettingreadyconnectingtoyourjournal = preparando. conectando con tu diario.

windows.ui.gettingthisdeviceready = preparando esta PC

windows.ui.globalshortcut = atajo global

windows.ui.goesalongwithit = va junto con esto

windows.ui.goodafternoon = buenas tardes

windows.ui.goodevening = buenas noches

windows.ui.goodmorning = buenos días

windows.ui.holdoneormoreofctrlalt = mantén uno o más de Ctrl, Alt, Shift o Win y presiona una tecla.

windows.ui.home = inicio

windows.ui.homex4e7a5cec = ‹ inicio

windows.ui.hourago = { $count ->
    [one] hace { $count } hora
    [many] hace { $count } horas
   *[other] hace { $count } horas
}

windows.ui.howoftentocheckforupdates = con qué frecuencia buscar actualizaciones

windows.ui.hrlefthrminleftboth = { $count } h { $count2 } min restantes

windows.ui.idle = en reposo

windows.ui.ifyouhaventyetaddthe = si todavía no lo has hecho, agrega la extensión de solstone a un navegador en esta PC. lo que compartes va a tu diario.

windows.ui.import = importar

windows.ui.importswaitonthisdeviceuntilyouconnectajournal = las importaciones esperan en este dispositivo hasta que conectes un diario.

windows.ui.importswaituntilyouconfirmyourjournalsmark = las importaciones esperan hasta que confirmes la marca de tu diario.

windows.ui.importswaituntilyourjournalisupdated = las importaciones esperan hasta que tu diario se actualice.

windows.ui.inputgain = ganancia de entrada

windows.ui.installing = instalando…

windows.ui.installingsolstone = instalando solstone { $v }…

windows.ui.inyourjournal = en tu diario.

windows.ui.inyourjournalchoosepairadeviceandpastethelinkher = en tu diario, elige "vincular un dispositivo" y pega el enlace aquí.

windows.ui.itinstallsthenexttimethesolstoneappstartsincludi = se instala la próxima vez que se inicie la app de solstone, incluso cuando inicias sesión en windows

windows.ui.itslargerthanyourjournaltakes = es más grande de lo que tu diario recibe

windows.ui.january = enero

windows.ui.journaldetails = detalles del diario

windows.ui.journallastreached = último contacto con el diario

windows.ui.journalneedsupdatebeforecantake = tu diario necesita una actualización antes de poder recibir estas notas. esperan en esta computadora.

windows.ui.journalsync = sincronización del diario

windows.ui.journalupdateneeded = el diario necesita una actualización

windows.ui.journalversion = versión del diario

windows.ui.july = julio

windows.ui.june = junio

windows.ui.keeponwhilepausedorscreenis = mantener activado durante la pausa o con la pantalla apagada

windows.ui.keepprivatewindowsoutofyourjournal = dejar fuera de tu diario las ventanas privadas: Edge y Firefox configurados en inglés

windows.ui.keptonthispc = conservado en esta PC

windows.ui.keptonthispcnotreachingyour = se conserva en esta PC, sin alcanzar tu diario ahora

windows.ui.label = sin vincular. vincula para sincronizar tu diario

windows.ui.lastchecked = última revisión

windows.ui.lastreached = última conexión

windows.ui.lasttakenin = última recepción

windows.ui.leftout = dejado fuera

windows.ui.lessthanaminuteleft = queda menos de un minuto

windows.ui.madebysolpbc = hecho por sol pbc.

windows.ui.march = marzo

windows.ui.mark = marca

windows.ui.markunavailable = marca·no disponible

windows.ui.markunavailablex5acbd11e = marca · no disponible

windows.ui.may = mayo

windows.ui.microphone = micrófono

windows.ui.microphonelevel = nivel del micrófono

windows.ui.microphones = micrófonos

windows.ui.minuteago = { $count ->
    [one] hace { $count } minuto
    [many] hace { $count } minutos
   *[other] hace { $count } minutos
}

windows.ui.moreaboutthis = más sobre esto

windows.ui.moreimports = más importaciones

windows.ui.movedown = bajar { $name }

windows.ui.moveup = subir { $name }

windows.ui.n1passwordbitwardenandtheotherpasswordmanager = 1Password, Bitwarden y las otras apps de administrador de contraseñas que figuran en configuración se dejan fuera, pero un administrador de contraseñas dentro de tu navegador no. puedes pausar en cualquier momento.

windows.ui.needsattentionword = necesita atención

windows.ui.needspermission = necesita permiso

windows.ui.next = siguiente

windows.ui.nextsave = próximo guardado

windows.ui.nobrowserconnectednow = ningún navegador conectado ahora

windows.ui.nocredentialaccess = sin acceso a credenciales

windows.ui.noexclusionsactive = ninguna exclusión activa

windows.ui.noidentitypresented = identidad no presentada

windows.ui.noinputdevice = ningún dispositivo de entrada

windows.ui.nokeywordsyet = todavía no hay palabras clave

windows.ui.nolocaljournaltoconnectto = no hay un diario local al que conectar

windows.ui.nomicrophonefound = no se encontró ningún micrófono.

windows.ui.nomicrophoneinputdevices = no hay dispositivos de entrada de micrófono

windows.ui.nomicrophoneisavailableconnectorenableoneinwindo = no hay ningún micrófono disponible. conecta o activa uno en la configuración de sonido de windows. la app de solstone empieza a usarlo por sí misma.

windows.ui.nonedisplay = ninguno

windows.ui.nootherappsrunning = no hay otras apps en ejecución

windows.ui.noroommorerightnownew = no hay lugar para más ahora. las páginas nuevas se vuelven a recibir cuando haya lugar.

windows.ui.noshortcutset = ningún atajo definido

windows.ui.nosoundoutputfound = no se encontró ninguna salida de sonido.

windows.ui.nosoundoutputisavailableconnectorenableoneinwind = no hay ninguna salida de sonido disponible. conecta o activa una en la configuración de sonido de windows. la app de solstone empieza a usarla por sí misma.

windows.ui.not = no

windows.ui.notanswered = sin respuesta

windows.ui.notavailable = no disponible

windows.ui.notavailablerightnow = no disponible ahora.

windows.ui.notcheckedforupdatesyet = todavía no se buscaron actualizaciones

windows.ui.notesjournal = { $value } de { $value2 } notas en tu diario

windows.ui.notgoingtoyourjournalrightnoweachonesayswhy = ahora no van a tu diario. cada uno dice por qué.

windows.ui.nothingexcludedyet = todavía no se excluyó nada

windows.ui.nothingfrombeforetoday = nada de antes de hoy

windows.ui.nothingwassavedonthisdevice = no se guardó nada en este dispositivo.

windows.ui.nothingyet = todavía no hay nada

windows.ui.nothingyetxec5a8da6 = todavía no hay nada.

windows.ui.notonthiscomputer = no está en esta computadora

windows.ui.notreachingyourjournalrightnow = ahora no se conecta con tu diario

windows.ui.notreported = no se informó

windows.ui.notyet = todavía no

windows.ui.nousagedataiseversentupdate = nunca se envían datos de uso. las búsquedas de actualizaciones solo traen el manifiesto de versiones.

windows.ui.november = noviembre

windows.ui.october = octubre

windows.ui.offline = sin conexión

windows.ui.offlinearg0waiting = sin conexión · { $arg0 } en espera

windows.ui.onlywhatthesolstoneappcantellisascreenshotcomesi = solo entra lo que la app de solstone puede distinguir como captura de pantalla.

windows.ui.onthesitesyouallowthesolstoneextensiontakesinthe = en los sitios que permites, la extensión de solstone recibe el texto de la página y el diseño aproximado, y todo va a tu diario.

windows.ui.onthisdevice = en este dispositivo

windows.ui.openfolder = abrir carpeta

windows.ui.opennavigation = abrir la navegación

windows.ui.opensettings = abrir configuración

windows.ui.opensoundsettings = abrir la configuración de sonido

windows.ui.openwindowssettings = abrir la configuración de windows

windows.ui.otherfilesinarg0staywheretheyare = los otros archivos en { $arg0 } se quedan donde están.

windows.ui.pageaddresses = direcciones de página

windows.ui.pageaddressesedgeandchrome = direcciones de página (Edge y Chrome)

windows.ui.pairagain = vincular de nuevo

windows.ui.pairedwith = vinculado con { $value }

windows.ui.pairing = vinculación

windows.ui.pairingcancellednothingthispchaskept = vinculación cancelada. pega un enlace nuevo de tu diario cuando puedas.

windows.ui.pairingdidntgothroughgeta = la vinculación no se completó. obtén un enlace de vinculación nuevo de tu diario y vuelve a intentarlo.

windows.ui.pairinglinkfromyourjournal = enlace de vinculación de tu diario

windows.ui.pairthispctoyourjournalto = vincula esta PC con tu diario para abrirlo

windows.ui.pairtodelivertoyourjournal = vincular para entregar a tu diario

windows.ui.pairwithyourjournalfirst = vincula con tu diario primero

windows.ui.partvaultcantberead = no se puede leer parte de esta bóveda. revisa los permisos de las carpetas que están adentro. el resto de la bóveda sigue yendo a tu diario.

windows.ui.paused = en pausa, { $count }

windows.ui.pausedarg0minleft = en pausa, { $arg0 } min restantes

windows.ui.pausedlessthanaminuteleft = en pausa, queda menos de un minuto

windows.ui.pauseduntilyouresume = en pausa hasta que reanudes

windows.ui.pausedwiththerestofthesolstone = en pausa con el resto de la app de solstone. pausar no frena lo que ya se recibió.

windows.ui.pausedword = en pausa

windows.ui.pausedxa7a9dc5b = en pausa

windows.ui.pauseresume = pausar / reanudar

windows.ui.pausesallsources = pausa todas las fuentes.

windows.ui.pending = { $value } pendientes

windows.ui.placeholder = pega un enlace de vinculación de tu diario

windows.ui.placeholder2 = una palabra clave, p. ej. banco

windows.ui.presented = presentada

windows.ui.pressyourshortcut = presiona tu atajo…

windows.ui.previewpaused = vista previa en pausa

windows.ui.privacy = privacidad

windows.ui.privatewindows = ventanas privadas

windows.ui.progresstext = paso { $stepnum } de { $totalsteps }

windows.ui.quitsolstone = salir de solstone

windows.ui.readytosetupword = disponible para configurar

windows.ui.recent = recientes

windows.ui.relaunchtoinstall = volver a abrir para instalar

windows.ui.remove = quitar { $value }

windows.ui.removearg0 = ¿quitar { $arg0 }?

windows.ui.removex7e5608ab = quitar

windows.ui.restartintake = reiniciar la recepción

windows.ui.retrying = { $value } reintentando

windows.ui.saveacopyofrecentlogs = guardar una copia de los registros recientes

windows.ui.savedonthisdevicearg0 = guardado en este dispositivo: { $arg0 }

windows.ui.savingonthisdevicearg0ofarg1 = guardando en este dispositivo: { $arg0 } de { $arg1 }

windows.ui.screen = pantalla

windows.ui.screenalsoaddstheappsyouuse = la pantalla también agrega a tu diario las apps que usas, con los títulos de ventana y las direcciones de página. puedes desactivar eso en configuración.

windows.ui.screenarg0computersoundarg1micarg2desktopactivit = pantalla: { $arg0 } · sonido de la computadora: { $arg1 } · micrófono: { $arg2 } · actividad de escritorio: { $arg3 }

windows.ui.screenpreview = vista previa de la pantalla

windows.ui.screenshots = capturas de pantalla

windows.ui.screenshotsalreadyinyourjournalarentbroughtinaga = las capturas de pantalla que ya están en tu diario no se vuelven a recibir.

windows.ui.screenshotsyoutakewhileitspauseddontcomein = las capturas de pantalla que tomas mientras está en pausa no entran.

windows.ui.segmentsthathavenotreachedyourjournal = los segmentos que todavía no han llegado a tu diario nunca se eliminan.

windows.ui.selecteddevice = dispositivo seleccionado

windows.ui.sendingtoyourjournal = enviando a tu diario.

windows.ui.sendingword = enviando

windows.ui.september = septiembre

windows.ui.set = definir

windows.ui.settingupword = configurando

windows.ui.setupconnectionchanged = la conexión de la configuración inicial cambió

windows.ui.setupisbusytryagainina = la configuración inicial está ocupada. vuelve a intentarlo en un momento.

windows.ui.setupprogresscouldnotbesaved = no se pudo guardar el progreso de la configuración inicial

windows.ui.setupsequenceisunavailableinthisbuild = la secuencia de configuración inicial no está disponible en esta compilación

windows.ui.setupunavailable = configuración inicial no disponible

windows.ui.sharesoundfromthiscomputer = compartir el sonido de esta PC

windows.ui.shareyourscreen = compartir tu pantalla

windows.ui.shortcut = atajo

windows.ui.shortcutsetbutturnedoff = atajo definido, pero desactivado

windows.ui.skip = omitir

windows.ui.sofar = hasta ahora

windows.ui.solstoneappcantreadvault = la app de solstone no puede leer esta bóveda. revisa que su carpeta esté ahí y que la unidad donde está esté conectada. nada cambia en tu diario mientras está fuera de alcance.

windows.ui.solstoneappcouldntsaveits = la app de solstone no pudo guardar su lugar para esta bóveda. revisa que esta computadora tenga espacio libre.

windows.ui.solstoneappdidntfindvault = la app de solstone no encontró una bóveda en esta computadora. elige su carpeta en su lugar.

windows.ui.solstoneapptakesnotesonevault = la app de solstone recibe las notas de una bóveda en esta computadora. todas van a tu diario, y luego cada una cuando cambia. tu bóveda se queda como está.

windows.ui.solstonearg0installsthenexttimethesolstoneappsta = solstone { $arg0 } se instala la próxima vez que se inicie la app de solstone, incluso cuando inicias sesión en windows

windows.ui.solstoneisavailable = solstone { $v } está disponible

windows.ui.solstoneisreadytoinstall = solstone { $v } está listo para instalar

windows.ui.solstonereadyinstall = solstone está listo para instalar

windows.ui.somenotescouldntgointo = algunas notas no pudieron ir a tu diario. la app de solstone las intenta de nuevo la próxima vez que se inicia.

windows.ui.somethingansweredpcatisnt = algo respondió a esta PC en { $value }, y no es el diario con el que esta PC está vinculada. esto puede ser otro dispositivo que ahora usa esa dirección, o tu diario, si se restableció su identidad. si la marca bajo "lo que respondió" coincide con la que tu diario muestra ahora, vincula esta PC de nuevo.

windows.ui.somethingansweredpcthroughrelayisn = algo respondió a esta PC a través del servidor de retransmisión, y no es el diario con el que esta PC está vinculada. esto da menos certeza que una conexión directa.

windows.ui.somethingotherthanyourjournalansweredcompare = algo respondió en lugar de tu diario

windows.ui.somethingyouwereaddingdidntfinishsavingadditagai = algo que estabas agregando no terminó de guardarse. agrégalo de nuevo.

windows.ui.sourcesetupisunfinished = la configuración inicial de la fuente no terminó

windows.ui.startfromtoday = empezar desde hoy

windows.ui.state = estado

windows.ui.status = estado

windows.ui.storage = almacenamiento

windows.ui.storagelocation = lugar de almacenamiento

windows.ui.storeappsallappsfromthemicrosoft = apps de la tienda (todas). las apps de Microsoft Store comparten una entrada, así que excluirla las excluye a todas

windows.ui.storedlocally = { $value } en almacenamiento local

windows.ui.storedonthispc = guardado en esta PC

windows.ui.sync = sincronización

windows.ui.text = { $name }. { $value2 }

windows.ui.text3 = { $name }, { $name2 } · { $value3 } { $value4 }

windows.ui.text6 = { $status }: { $reason }

windows.ui.textcontent = llegó otro enlace de vinculación: úsalo u ocúltalo.

windows.ui.textcontent2 = vinculando…

windows.ui.textcontent4 = borrar el atajo

windows.ui.textcontent5 = { $level }×

windows.ui.textfilestxt = archivos de texto (*.txt)

windows.ui.thatcomboisinusebyanother = esa combinación la usa otra app. elige otra.

windows.ui.thatdidnotfinish = eso no terminó

windows.ui.thatfolderisntobsidianvault = esa carpeta no es una bóveda de Obsidian, así que no se envió nada.

windows.ui.thatfoldersnamecant = el nombre de esa carpeta no puede ser el nombre de una bóveda en tu diario. cambia el nombre de la carpeta y vuelve a intentarlo.

windows.ui.thatpairinglinkisntonethe = ese enlace de vinculación no es uno que la app de solstone pueda leer. obtén un enlace de vinculación nuevo de tu diario y vuelve a intentarlo.

windows.ui.thearg0folderisntonthispcanymore = la carpeta { $arg0 } ya no está en esta PC.

windows.ui.thecopyisalreadygonesothisonlyclearsitfromhere = la copia ya no está, así que esto solo la quita de aquí.

windows.ui.thecopyonthisdeviceismissing = falta la copia en este dispositivo

windows.ui.thedownloaddidnotfinish = { $leading }· la descarga no terminó

windows.ui.thelast30days = los últimos 30 días

windows.ui.thelast6months = los últimos 6 meses

windows.ui.thelastcheckforupdatesfailed = { $leading }· no se pudo buscar actualizaciones la última vez

windows.ui.thelastyear = el último año

windows.ui.thepairingwindowclosedgetanew = la ventana de vinculación se cerró. obtén un enlace de vinculación nuevo de tu diario y luego vuelve a intentarlo.

windows.ui.thereisntenoughroomonthisdevice = no hay suficiente espacio en este dispositivo

windows.ui.theserulesareactivenowbutcouldn = estas reglas están activas ahora, pero no se pudieron guardar, así que puede que no sobrevivan a un reinicio.

windows.ui.thesolstoneappusesonemicrophoneat = la app de solstone usa un micrófono a la vez. define cuál, y cuánto reforzarlo.

windows.ui.thetopenabledmicrophoneisuseduse = se usa el micrófono activado de más arriba. usa ↑ ↓ para definir el orden; la app de solstone pasa al siguiente si uno no está disponible.

windows.ui.theupdatecouldntbeinstalled = { $leading }· no se pudo instalar la actualización

windows.ui.thisbuildcantupdateitself = esta compilación no puede actualizarse sola

windows.ui.thisconnectioncouldnotbesavedtry = no se pudo guardar esta conexión. vuelve a intentarlo, o pega un enlace de vinculación de tu diario.

windows.ui.thisconnectioncouldnotfinishtryagain = no se pudo terminar esta conexión. vuelve a intentarlo, o pega un enlace de vinculación nuevo de tu diario.

windows.ui.thisconnectioncouldnotfinishtryagain2 = no se pudo terminar esta conexión. vuelve a intentarlo.

windows.ui.thiscouldntbereadadditagain = no se pudo leer esto. agrégalo de nuevo

windows.ui.thisdevicecantconvertthisphoto = este dispositivo no puede convertir esta foto

windows.ui.thisdevicecouldntopenitssaved = esta PC no pudo abrir su conexión guardada con tu diario. lo que espera para enviarse sigue aquí. vincula de nuevo para reconectar.

windows.ui.thisdeviceisnotpairedwithyourjournalimportswaith = este dispositivo no está vinculado con tu diario. las importaciones esperan aquí hasta que conectes un diario.

windows.ui.thisdeviceisntonanetwork = esta PC no está en una red. la vinculación tiene que conectar directamente con tu diario, así que únete a la misma wi-fi que tu diario y vuelve a intentarlo. todo lo que la app de solstone recibió está en esta PC y se sincroniza cuando vuelvas a conectar.

windows.ui.thisjournallinkisntvalid = este enlace del diario no es válido.

windows.ui.thislinkcouldnotbedismissedtry = no se pudo ocultar este enlace. vuelve a intentarlo.

windows.ui.thislinkcouldnotbeusedtry = no se pudo usar este enlace. vuelve a intentarlo.

windows.ui.thislistappliestoactivitytoo = esta lista también se aplica a la actividad

windows.ui.thisonlytakesamoment = esto solo toma un momento

windows.ui.thispc = esta PC

windows.ui.thispccantconnecttoyour = esta PC no puede conectar con tu diario con su vinculación guardada. obtén un enlace de vinculación nuevo de tu diario y vincula de nuevo.

windows.ui.thispccouldntworkoutyour = esta PC no pudo determinar la marca de tu diario, así que no hay nada que comparar. continúa solo si tienes la certeza de que el enlace vino de tu diario.

windows.ui.thisreadsthewindowtitleandmatches = esto lee el título de la ventana e identifica las ventanas privadas en Edge y Firefox. eso se revisó en Edge 153 y Firefox 156, cada uno configurado en inglés; una actualización del navegador u otro idioma puede cambiar un título, y entonces una ventana privada de ahí puede llegar a tu diario. Chrome 153 y Brave 153 no muestran el modo privado en sus títulos, y los otros navegadores no se identifican, así que sus ventanas privadas llegan a tu diario. para dejar fuera de tu diario todas las ventanas de un navegador, agrega ese navegador a las apps excluidas de abajo.

windows.ui.thisremovesthecopyonthisdevicesoitwontreachyourj = esto quita la copia de este dispositivo, así que no llegará a tu diario.

windows.ui.thisremovesthecopyonthisdeviceyourjournalmaystil = esto quita la copia de este dispositivo. tu diario todavía puede recibir la copia que ya tiene.

windows.ui.thissetuplinkcouldnotbeused = no se pudo usar este enlace de configuración inicial. vuelve a tu diario para obtener un enlace nuevo, o pega aquí un enlace de vinculación.

windows.ui.thisstepcouldnotfinishtryagain = no se pudo terminar este paso. vuelve a intentarlo.

windows.ui.thisstepcouldntfinishtryagain = no se pudo terminar este paso. vuelve a intentarlo.

windows.ui.titlekeywords = palabras clave del título

windows.ui.titlekeywordtoexclude = palabra clave del título para excluir

windows.ui.toolargebring = demasiado grande para recibir

windows.ui.trynow = intentar ahora

windows.ui.trynowarg0 = intentar ahora, { $arg0 }

windows.ui.tryopeningthelinkagain = intenta abrir el enlace de nuevo.

windows.ui.turningoffstopsbringingchangeswhat = desactivar esto detiene la recepción de los cambios. lo que ya está en tu diario se queda ahí, y lo que espera se queda en esta computadora hasta que lo vuelvas a activar.

windows.ui.turnitonanytime = actívalo cuando quieras.

windows.ui.turnoff = desactivar

windows.ui.turnon = activar { $accessiblename }

windows.ui.unavailable = no disponible

windows.ui.unavailableword = no disponible

windows.ui.unknown = desconocido

windows.ui.unknownjournals = diarios desconocidos

windows.ui.untiliresume = hasta que yo reanude

windows.ui.usethislink = usar este enlace

windows.ui.usevault = usar esta bóveda

windows.ui.value15minutes = 15 minutos

windows.ui.value1hour = 1 hora

windows.ui.value30minutes = 30 minutos

windows.ui.vaults = bóvedas

windows.ui.vaultscomputer = bóvedas en esta computadora

windows.ui.vaultwithnamealreadyjournal = ya hay una bóveda con este nombre en tu diario

windows.ui.viewdetails = ver detalles

windows.ui.waiting = en espera

windows.ui.waitingforajournalupdateyourscreenshotswaitonthi = en espera de una actualización del diario. tus capturas de pantalla esperan en esta PC.

windows.ui.waitingforyouranswer = en espera de tu respuesta

windows.ui.waitingforyoutoconfirmyourjournalsmark = en espera de que confirmes la marca de tu diario

windows.ui.waitingjournalupdate = en espera de una actualización del diario

windows.ui.waitingreachjournal = en espera de conectar con tu diario

windows.ui.waitingsync = { $value } en espera de sincronizar

windows.ui.waitingtogotoyourjournal = en espera de ir a tu diario

windows.ui.waitingtogotoyourjournalx5d789db5 = en espera de ir a tu diario.

windows.ui.waitingtosync = en espera de sincronizar

windows.ui.waitingword = en espera

windows.ui.whatanswered = lo que respondió

windows.ui.whatsalreadyinyourjournalstayswhenyoupauseorturn = lo que ya está en tu diario se queda cuando pausas o desactivas esto.

windows.ui.whatyousharestaysonthispc = lo que compartes se queda en esta PC hasta que va a tu diario en tus propios dispositivos. tu diario es siempre privado, solo tuyo.

windows.ui.whatyouturnongoesintoyour = lo que activas va a tu diario.

windows.ui.whilethisisonthesolstoneapptakesinsoundandallofi = mientras esto está activado, la app de solstone recibe el sonido, y todo va a tu diario.

windows.ui.whilethisisonthesolstoneapptakesinthescreenshots = mientras esto está activado, la app de solstone recibe las capturas de pantalla que tomas en esta PC, y tu diario guarda cada una en el minuto en que la tomaste.

windows.ui.whilethisisonthesolstoneapptakesinwhatsonyourscr = mientras esto está activado, la app de solstone recibe lo que hay en tu pantalla, y todo va a tu diario.

windows.ui.windowsfromtheseappsneverreachyour = las ventanas de estas apps nunca llegan a tu diario.

windows.ui.windowswhosetitlecontainsawordyou = las ventanas cuyo título contiene una palabra que eliges nunca llegan a tu diario.

windows.ui.withactivity = con actividad

windows.ui.withcomputersound = con sonido de la computadora

windows.ui.yesterday = ayer

windows.ui.youcanturneachsourceonor = puedes activar o desactivar cada fuente en fuentes.

windows.ui.youchoosewhichsourcesareon = tú eliges qué fuentes están activadas.

windows.ui.youllseehowmanyscreenshotstherearebeforeanycomei = verás cuántas capturas de pantalla hay antes de que entre alguna.

windows.ui.your = tu

windows.ui.yourjournal = tu · diario

windows.ui.yourjournalcanttakethiskindoffile = tu diario no puede recibir este tipo de archivo

windows.ui.yourjournalcouldntacceptthis = tu diario no pudo aceptar esto.

windows.ui.yourjournaldoesntsupportthismove = tu diario todavía no admite este traslado. lo que espera para enviarse se queda en esta PC. actualiza tu diario y luego vuelve a intentarlo.

windows.ui.yourjournalneedsanewlink = tu diario necesita un enlace nuevo

windows.ui.yourjournalnotsetupyet = tu diario, todavía sin configurar

windows.ui.yourjournalopenedbutsetupprogresscould = tu diario se abrió, pero no se pudo guardar el avance de la configuración inicial. vuelve a intentarlo.

windows.ui.yourjournalshowsthissamemarkbeside = tu diario muestra esta misma marca junto al enlace de vinculación. debe coincidir, exactamente.

windows.ui.yourjournalsmarkunavailablerightnow = la marca de tu diario, no disponible ahora

windows.ui.yourjournalstoppedacceptingthisdeviceimportswait = tu diario dejó de aceptar este dispositivo. las importaciones esperan aquí hasta que vuelvas a vincular.

windows.ui.yourshortcut = tu atajo

windows.ui.yousaidthismarkdoesntmatch = dijiste que esta marca no coincide con la que muestra tu diario, así que esta PC no está vinculada. puede que hayas pegado el enlace equivocado, o que algo no esté bien. pega un enlace nuevo de tu diario, o envía un correo a support@solstone.app y te ayudamos.
