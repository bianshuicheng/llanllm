; LLanLLM 被控端安装器钩子（Tauri NSIS installerHooks，perMachine 提权运行）
; 安装完成：
;   1. 读取安装器自身尾部的注入配置（LLANCFG2：[json][ip][magic][ip_len][json_len]）
;      写出 config.json + config-ip.txt
;   2. 据主控 IP 自动添加防火墙规则（仅允许主控访问 RPC 50052）——无需用户手动放行
; 卸载时：删除防火墙规则、杀掉相关进程、清理配置文件。
; 注意：NSIS 只提供 $0-$9 / $R0-$R9 变量。

!macro NSIS_HOOK_POSTINSTALL
  ClearErrors
  FileOpen $0 "$EXEPATH" r
  ${IfNot} ${Errors}
    FileSeek $0 -12 END
    ClearErrors
    ; 读 8 字节 magic → $1
    StrCpy $1 ""
    ${For} $2 1 8
      FileReadByte $0 $3
      IntFmt $3 "%c" $3
      StrCpy $1 "$1$3"
    ${Next}
    ${If} $1 == "LLANCFG2"
      ; 尾部布局：[json][ip][magic 8][ip_len 4][json_len 4]（两个长度均为 u32 LE）
      FileSeek $0 -4 END
      FileReadByte $0 $6     ; json_len 低字节
      FileReadByte $0 $2     ; json_len 高字节
      IntOp $4 $2 * 256
      IntOp $4 $4 + $6
      FileReadByte $0 $6     ; ip_len 低字节
      FileReadByte $0 $2     ; ip_len 高字节
      IntOp $5 $2 * 256
      IntOp $5 $5 + $6
      ; 回退到配置区起点
      IntOp $6 $4 + $5
      IntOp $6 $6 + 12
      FileSeek $0 -$6 END
      ; 写 config.json
      FileOpen $R0 "$INSTDIR\config.json" w
      ${IfNot} ${Errors}
        ${For} $2 1 $4
          FileReadByte $0 $3
          FileWriteByte $R0 $3
        ${Next}
        FileClose $R0
      ${EndIf}
      ; 写 config-ip.txt
      FileOpen $R1 "$INSTDIR\config-ip.txt" w
      ${IfNot} ${Errors}
        ${For} $2 1 $5
          FileReadByte $0 $3
          FileWriteByte $R1 $3
        ${Next}
        FileClose $R1
      ${EndIf}
    ${EndIf}
    FileClose $0
  ${EndIf}

  ; 防火墙：安装器已是管理员权限，直接加规则（主控 IP 优先，兜底本地子网）
  StrCpy $R2 "localsubnet"
  ${If} ${FileExists} "$INSTDIR\config-ip.txt"
    ClearErrors
    FileOpen $R1 "$INSTDIR\config-ip.txt" r
    ${IfNot} ${Errors}
      FileRead $R1 $R2
      FileClose $R1
      StrCpy $R2 $R2 15
    ${EndIf}
  ${EndIf}
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Agent RPC"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LLanLLM Agent RPC" dir=in action=allow protocol=TCP localport=50052 remoteip=$R2 profile=private,domain'
  Pop $2
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::ExecToLog 'taskkill /F /IM ggml-rpc-server.exe'
  Pop $2
  nsExec::ExecToLog 'taskkill /F /IM llanllm-agent.exe'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Agent RPC"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Agent CTL"'
  Pop $2
  Delete "$INSTDIR\config.json"
  Delete "$INSTDIR\config-ip.txt"
!macroend
