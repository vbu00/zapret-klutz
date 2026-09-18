; Хуки установщика Klutz (NSIS). Подключаются Tauri через
; bundle.windows.nsis.installerHooks; макросы вставляются в installer.nsi
; в четырёх точках: до/после установки и до/после удаления.
;
; Зачем: чистая установка и удаление должны оставлять систему в порядке.
; Шаблон Tauri закрывает только klutz.exe и снимает только свои файлы и
; ключи, а у Klutz есть хвосты вне папки установки — процесс прокси, задача
; Планировщика, служба zapret, записи прежних версий в реестре.
;
; Регистры $R0–$R7 здесь свободны: шаблон Tauri пользуется $0–$9.

!macro NSIS_HOOK_PREINSTALL
  ; Tauri закрывает klutz.exe, но не дочерний прокси. Тот лежит в
  ; $INSTDIR\bin и, пока работает, не даёт себя перезаписать — установка
  ; поверх спотыкалась на «файл занят».
  nsExec::ExecToLog 'taskkill /F /IM TgWsProxyHeadless.exe'
  Pop $R0

  ; Записи прежних версий Klutz в «Программы и компоненты». До 1.2.3
  ; установка шла в профиль пользователя и писалась в HKCU; теперь — для
  ; всех, в HKLM. Старая запись остаётся и показывает второй «Klutz», а если
  ; старая копия стояла в этой же папке — её деинсталлятор сейчас будет
  ; перезаписан нашим, и запись ведёт не туда.
  ; Удаляем запись, только если она мёртвая: указывает на нашу папку или её
  ; деинсталлятора уже нет на диске. Живую копию в другом месте не трогаем.
  StrCpy $R0 0
  klutz_stale_loop:
    EnumRegKey $R1 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall" $R0
    StrCmp $R1 "" klutz_stale_done
    ReadRegStr $R2 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "DisplayName"
    StrCpy $R3 $R2 5
    StrCmp $R3 "Klutz" 0 klutz_stale_next

    ; InstallLocation Tauri пишет в кавычках.
    ReadRegStr $R4 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "InstallLocation"
    StrCpy $R5 $R4 1
    StrCmp $R5 '"' 0 +2
      StrCpy $R4 $R4 "" 1
    StrCpy $R5 $R4 1 -1
    StrCmp $R5 '"' 0 +2
      StrCpy $R4 $R4 -1
    StrCmp $R4 "" +2
    StrCmp $R4 "$INSTDIR" klutz_stale_delete

    ; Путь к деинсталлятору из UninstallString: в кавычках или до первого
    ; пробела.
    ReadRegStr $R4 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R1" "UninstallString"
    StrCmp $R4 "" klutz_stale_delete
    StrCpy $R5 $R4 1
    StrCmp $R5 '"' 0 klutz_stale_nospace
      StrCpy $R6 1
      klutz_stale_quote:
        StrCpy $R5 $R4 1 $R6
        StrCmp $R5 "" klutz_stale_cut
        StrCmp $R5 '"' klutz_stale_cut
        IntOp $R6 $R6 + 1
        Goto klutz_stale_quote
      klutz_stale_cut:
        IntOp $R7 $R6 - 1
        StrCpy $R4 $R4 $R7 1
        Goto klutz_stale_check
    klutz_stale_nospace:
      StrCpy $R6 0
      klutz_stale_space:
        StrCpy $R5 $R4 1 $R6
        StrCmp $R5 "" klutz_stale_cut2
        StrCmp $R5 " " klutz_stale_cut2
        IntOp $R6 $R6 + 1
        Goto klutz_stale_space
      klutz_stale_cut2:
        StrCpy $R4 $R4 $R6
    klutz_stale_check:
    IfFileExists "$R4" klutz_stale_next klutz_stale_delete

    klutz_stale_delete:
      DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R1"
      ; После удаления индексы сдвигаются — перечитываем с того же места.
      Goto klutz_stale_loop
    klutz_stale_next:
      IntOp $R0 $R0 + 1
      Goto klutz_stale_loop
  klutz_stale_done:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; klutz.exe всегда поднимается до администратора (манифест). Если он лежит
  ; в папке, куда может писать обычный пользователь, его можно подменить, и
  ; Windows сама поднимет подменённый код до администратора. Program Files
  ; защищён по умолчанию, но человек волен выбрать другую папку (или
  ; установщик подхватит прежнюю), например C:\Programs\Klutz — там у
  ; «Прошедших проверку» есть право записи, унаследованное от корня диска.
  ; Ставим права как у Program Files: администраторы и система — полный
  ; доступ, пользователи — чтение и запуск. Трогаем только папку с именем
  ; продукта: чужую общую папку так резать нельзя.
  StrCpy $R0 $INSTDIR "" -6
  StrCmp $R0 "\Klutz" 0 klutz_acl_done
  StrLen $R1 $PROGRAMFILES64
  StrCpy $R2 $INSTDIR $R1
  StrCmp $R2 $PROGRAMFILES64 klutz_acl_done
  StrLen $R1 $PROGRAMFILES
  StrCpy $R2 $INSTDIR $R1
  StrCmp $R2 $PROGRAMFILES klutz_acl_done
    ; SID вместо имён: на русской Windows группы называются иначе.
    ; S-1-5-32-544 — Администраторы, S-1-5-18 — SYSTEM, S-1-5-32-545 — Пользователи.
    nsExec::ExecToLog 'icacls "$INSTDIR" /setowner "*S-1-5-32-544" /T /C /Q'
    Pop $R0
    nsExec::ExecToLog 'icacls "$INSTDIR" /inheritance:r /grant:r "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-545:(OI)(CI)RX" /T /C /Q'
    Pop $R0
  klutz_acl_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; /UPDATE — установка поверх, а не удаление: всё, что человек включил,
  ; должно пережить обновление. Раньше этот хук выполнялся и тогда, и после
  ; обновления пропадали автозапуск и служба «держать включённым».
  ${If} $UpdateMode <> 1

  ; Правило брандмауэра «Discord без QUIC» и строки Klutz в hosts убирает сам
  ; Klutz (keep.rs): логика там, где её можно проверить тестами.
  nsExec::ExecToLog '"$INSTDIR\${MAINBINARYNAME}.exe" --cleanup'
  Pop $R0

  ; Автозапуск через Планировщик: иначе после удаления задача остаётся и
  ; при каждом входе пытается запустить несуществующий klutz.exe.
  nsExec::ExecToLog 'schtasks /Delete /TN "Klutz-Autostart" /F'
  Pop $R0
  ; Прокси Telegram — дочерний процесс, Tauri закрывает только klutz.exe.
  nsExec::ExecToLog 'taskkill /F /IM TgWsProxyHeadless.exe'
  Pop $R0

  ; Служба zapret, поставленная из Klutz, указывает на релиз в его папке
  ; данных. После удаления (тем более с галкой «удалить данные») она
  ; остаётся и при каждой загрузке падает «служба завершена неожиданно».
  ; Чужую службу — с релизом в другом месте — не трогаем, как и её winws.
  nsExec::ExecToStack 'cmd.exe /c reg query "HKLM\System\CurrentControlSet\Services\zapret" /v ImagePath | find /i "\${BUNDLEID}\"'
  Pop $R0
  Pop $R1
  StrCmp $R0 "0" 0 klutz_svc_foreign
    nsExec::ExecToLog 'net stop zapret'
    Pop $R0
    nsExec::ExecToLog 'sc delete zapret'
    Pop $R0
    nsExec::ExecToLog 'taskkill /F /IM winws.exe'
    Pop $R0
    Goto klutz_svc_done
  klutz_svc_foreign:
  ; Службы нет вовсе — значит работающий winws.exe запущен самим Klutz;
  ; после удаления ему держать обход незачем, а занятые файлы релиза мешают
  ; удалить данные.
  nsExec::ExecToStack 'sc query zapret'
  Pop $R0
  Pop $R1
  StrCmp $R0 "0" klutz_svc_done
    nsExec::ExecToLog 'taskkill /F /IM winws.exe'
    Pop $R0
  klutz_svc_done:

  ${EndIf}
!macroend
