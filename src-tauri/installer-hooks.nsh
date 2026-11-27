; LLanLLM 被控端安装器钩子（Tauri NSIS installerHooks，perMachine 提权运行）
;
; 注入格式 v3（与 src-tauri/src/common/bootstrap.rs 同步定义，尾部有单测钉死契约）：
;   [原始安装器字节][config JSON(utf8)][主控IP ascii][json_len 6位十进制][ip_len 4位十进制]["LLANCFG3"]
;   定长尾部 18 字节、magic 在最末 → 从文件末尾倒读即可同时完成识别与定位，
;   不需要任何偏移量推算（v2 的 magic 不在末尾、钩子偏移写错，导致 v0.1.0 的
;   「导出→安装→自动配对」从未生效）。
;
; POSTINSTALL：
;   1. 识别尾部 → 写出 config.json（被控端启动时读取，凭引导码自动入网）
;   2. 据主控 IP 添加防火墙规则：RPC 50052 仅允许主控 IP，profile=any（全部网络类型）
; PREUNINSTALL：杀进程、删规则、删注入配置文件（先于模板删除文件执行）。
; 注意：NSIS 只提供 $0-$9 / $R0-$R9 变量。

; 注册 StrFunc 的行尾裁剪宏（模板已 include StrFunc.nsh）
${StrTrimNewLines}

; 读安装器尾部定长 18 字节：$1=json_len(6位) $2=ip_len(4位) $3=magic(8字符)
; 输入：$0 = 文件句柄，已 FileSeek -18 END
!macro LLanReadTail
  StrCpy $1 ""
  ${For} $R0 1 6
    FileReadByte $0 $R1
    IntFmt $R1 "%c" $R1
    StrCpy $1 "$1$R1"
  ${Next}
  StrCpy $2 ""
  ${For} $R0 1 4
    FileReadByte $0 $R1
    IntFmt $R1 "%c" $R1
    StrCpy $2 "$2$R1"
  ${Next}
  StrCpy $3 ""
  ${For} $R0 1 8
    FileReadByte $0 $R1
    IntFmt $R1 "%c" $R1
    StrCpy $3 "$3$R1"
  ${Next}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ClearErrors
  FileOpen $0 "$EXEPATH" r
  ${IfNot} ${Errors}
    FileSeek $0 -18 END
    !insertmacro LLanReadTail
    ${If} $3 == "LLANCFG3"
      IntOp $4 $1 + 0   ; json_len（十进制 ASCII → 数字）
      IntOp $5 $2 + 0   ; ip_len
      IntOp $6 $4 + $5
      IntOp $6 $6 + 18
      FileSeek $0 -$6 END
      ; 写 config.json（引导配置，被控端启动读取）
      FileOpen $R2 "$INSTDIR\config.json" w
      ${IfNot} ${Errors}
        ${For} $R0 1 $4
          FileReadByte $0 $R1
          FileWriteByte $R2 $R1
        ${Next}
        FileClose $R2
      ${EndIf}
      ; 写 config-ip.txt（防火墙规则用）
      FileOpen $R3 "$INSTDIR\config-ip.txt" w
      ${IfNot} ${Errors}
        ${For} $R0 1 $5
          FileReadByte $0 $R1
          FileWriteByte $R3 $R1
        ${Next}
        FileClose $R3
      ${EndIf}
    ${EndIf}
    FileClose $0
  ${EndIf}

  ; 防火墙：RPC 50052 仅允许主控 IP（无注入 IP 时兜底本网段），全部网络类型
  StrCpy $R4 "localsubnet"
  ${If} ${FileExists} "$INSTDIR\config-ip.txt"
    ClearErrors
    FileOpen $R3 "$INSTDIR\config-ip.txt" r
    ${IfNot} ${Errors}
      FileRead $R3 $R4
      FileClose $R3
      ${StrTrimNewLines} $R4 $R4
    ${EndIf}
  ${EndIf}
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Agent RPC"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LLanLLM Agent RPC" dir=in action=allow protocol=TCP localport=50052 remoteip=$R4 profile=any'
  Pop $2
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'taskkill /F /IM ggml-rpc-server.exe'
  Pop $2
  nsExec::ExecToLog 'taskkill /F /IM ${MAINBINARYNAME}.exe'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Agent RPC"'
  Pop $2
  Delete "$INSTDIR\config.json"
  Delete "$INSTDIR\config-ip.txt"
!macroend
