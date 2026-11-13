; LLanLLM 主控端安装器钩子（Tauri NSIS installerHooks，perMachine 提权运行）
; 安装完成：自动放行主控端口（UDP 47890 心跳入站 + TCP 47891 被控控制连接入站）
;   —— 用户无需手动配置防火墙
; 卸载时：删除防火墙规则、杀掉相关子进程。

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master Discovery"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master CTL"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LLanLLM Master Discovery" dir=in action=allow protocol=UDP localport=47890 profile=private,domain'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="LLanLLM Master CTL" dir=in action=allow protocol=TCP localport=47891 profile=private,domain'
  Pop $2
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::ExecToLog 'taskkill /F /IM llama-server.exe'
  Pop $2
  nsExec::ExecToLog 'taskkill /F /IM ggml-rpc-server.exe'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master Discovery"'
  Pop $2
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="LLanLLM Master CTL"'
  Pop $2
!macroend
