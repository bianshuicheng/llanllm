; LLanLLM 主控端安装器钩子（Tauri NSIS installerHooks，perMachine 提权运行）
; POSTINSTALL：自动放行主控端口（UDP 47890 心跳入站 + TCP 47891 被控控制连接入站）
;   profile=any：Windows 把家庭/办公网判成「公用网络」时同样生效（v0.1.0 只写了
;   private,domain 导致公用网络下完全失联）；入站来源限 localsubnet，不扩大暴露面。
; PREUNINSTALL：杀掉 llama-server / rpc-server 子进程并删除防火墙规则
;   （放在 PREINSTALL 之前的卸载钩子里，模板随后 RMDir 才能删净安装目录）。
; 注意：NSIS 只提供 $0-$9 / $R0-$R9 变量。

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master Discovery"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master CTL"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LLanLLM Master Discovery" dir=in action=allow protocol=UDP localport=47890 remoteip=localsubnet profile=any'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LLanLLM Master CTL" dir=in action=allow protocol=TCP localport=47891 remoteip=localsubnet profile=any'
  Pop $2
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'taskkill /F /IM llama-server.exe'
  Pop $2
  nsExec::ExecToLog 'taskkill /F /IM ggml-rpc-server.exe'
  Pop $2
  nsExec::ExecToLog 'taskkill /F /IM ${MAINBINARYNAME}.exe'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master Discovery"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master CTL"'
  Pop $2
!macroend
