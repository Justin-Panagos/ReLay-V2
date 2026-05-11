rule EICAR_Test {
    meta:
        description = "EICAR antivirus test file"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $s = "EICAR-STANDARD-ANTIVIRUS-TEST-FILE"
    condition:
        $s
}

rule Suspicious_Dropper {
    meta:
        description = "Generic dropper pattern: PE with embedded cmd.exe invocation"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "This program cannot be run in DOS mode"
        $b = { 4D 5A }
        $c = "cmd.exe /c" nocase
    condition:
        $b at 0 and $a and $c
}

rule Mimikatz_Common_Strings {
    meta:
        description = "Detects common strings found in the Mimikatz credential dumper"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "sekurlsa::logonpasswords" nocase
        $b = "lsadump::sam" nocase
        $c = "privilege::debug" nocase
        $d = "mimikatz" nocase
    condition:
        any of them
}

rule CobaltStrike_Beacon_Strings {
    meta:
        description = "Detects CobaltStrike beacon artifacts"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "ReflectiveLoader"
        $b = "beacon.x64.dll" nocase
        $c = "beacon.dll" nocase
        $d = "%02d/%02d/%02d %02d:%02d:%02d"
    condition:
        2 of them
}

rule UPX_Packed_Executable {
    meta:
        description = "Detects UPX-packed executables"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $mz = { 4D 5A }
        $upx0 = "UPX0"
        $upx1 = "UPX1"
        $upx_sig = "UPX!"
    condition:
        $mz at 0 and ($upx0 or $upx1) and $upx_sig
}

rule PowerShell_DownloadCradle {
    meta:
        description = "Detects PowerShell download cradles commonly used in malware stages"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $iex = "IEX" nocase
        $dl1 = "DownloadString" nocase
        $dl2 = "DownloadFile" nocase
        $wc  = "WebClient" nocase
        $b64 = "FromBase64String" nocase
    condition:
        $iex and ($dl1 or $dl2) and ($wc or $b64)
}

rule AMSI_Bypass_Patch {
    meta:
        description = "Detects common AMSI bypass strings and patching patterns"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "AmsiScanBuffer" nocase
        $b = "amsi.dll" nocase
        $c = "amsiInitFailed" nocase
        $d = "AmsiOpenSession" nocase
    condition:
        ($a and $b) or $c or ($a and $d)
}

rule Ransomware_Note_Keywords {
    meta:
        description = "Detects ransomware note keywords embedded in executables"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $mz = { 4D 5A }
        $r1 = "your files have been encrypted" nocase
        $r2 = "to decrypt your files" nocase
        $r3 = "send bitcoin" nocase
        $r4 = "your personal id" nocase
        $r5 = ".onion" nocase
        $r6 = "pay the ransom" nocase
    condition:
        $mz at 0 and 2 of ($r1, $r2, $r3, $r4, $r5, $r6)
}

rule Meterpreter_Strings {
    meta:
        description = "Detects Metasploit Meterpreter artifact strings"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "meterpreter" nocase
        $b = "stdapi_" nocase
        $c = "ReflectiveDllInjection" nocase
        $d = "METERPRETER_TRANSPORT_TCP" nocase
    condition:
        any of them
}

rule Keylogger_Hook_API {
    meta:
        description = "Detects PE imports consistent with keylogger functionality"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $mz  = { 4D 5A }
        $hook = "SetWindowsHookEx" nocase
        $log  = "keylog" nocase
        $vk   = "GetAsyncKeyState" nocase
    condition:
        $mz at 0 and $hook and ($log or $vk)
}

rule Self_Delete_On_Exit {
    meta:
        description = "Detects malware that schedules its own deletion via cmd.exe on exit"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $mz   = { 4D 5A }
        $del1 = "cmd.exe /c del" nocase
        $del2 = "cmd /c del " nocase
    condition:
        $mz at 0 and ($del1 or $del2)
}

rule PHP_Webshell_Generic {
    meta:
        description = "Detects common PHP webshell patterns: eval + base64 + system call + user input"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $php  = "<?php" nocase
        $eval = "eval(" nocase
        $b64  = "base64_decode(" nocase
        $s1   = "system(" nocase
        $s2   = "exec(" nocase
        $s3   = "passthru(" nocase
        $s4   = "shell_exec(" nocase
        $p1   = "$_POST" nocase
        $p2   = "$_GET" nocase
        $p3   = "$_REQUEST" nocase
    condition:
        $php and $eval and $b64 and (1 of ($s1,$s2,$s3,$s4)) and (1 of ($p1,$p2,$p3))
}

rule WMI_Lateral_Movement {
    meta:
        description = "Detects WMI-based lateral movement patterns"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "Win32_Process" nocase
        $b = "winmgmts:" nocase
        $c = "wmic" nocase
        $d = "\\root\\cimv2" nocase
        $e = "Create" nocase
    condition:
        ($a and $e) or ($b and $e) or ($c and $d)
}

rule Suspicious_Process_Injection {
    meta:
        description = "Detects PE imports consistent with process injection (VirtualAllocEx + WriteProcessMemory + remote thread)"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $mz  = { 4D 5A }
        $va  = "VirtualAllocEx" nocase
        $wp  = "WriteProcessMemory" nocase
        $crt = "CreateRemoteThread" nocase
        $op  = "OpenProcess" nocase
    condition:
        $mz at 0 and $va and $wp and ($crt or $op)
}

rule ETW_Patch_Strings {
    meta:
        description = "Detects ETW (Event Tracing for Windows) patching attempts used to blind defenders"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "EtwEventWrite" nocase
        $b = "ntdll.dll" nocase
        $c = "EtwpCreateEtwThread" nocase
        $d = "NtTraceEvent" nocase
    condition:
        ($a and $b) or $c or ($a and $d)
}

rule Office_Macro_Suspicious {
    meta:
        description = "Detects suspicious Office macro patterns: auto-open + shell execution + object creation"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $open1 = "AutoOpen" nocase
        $open2 = "Document_Open" nocase
        $open3 = "Workbook_Open" nocase
        $sh1   = "Shell(" nocase
        $sh2   = "PowerShell" nocase
        $sh3   = "WScript" nocase
        $obj1  = "CreateObject" nocase
        $obj2  = "MSXML2.XMLHTTP" nocase
    condition:
        (1 of ($open1,$open2,$open3)) and (1 of ($sh1,$sh2,$sh3)) and (1 of ($obj1,$obj2))
}

rule Browser_Credential_Theft_Paths {
    meta:
        description = "Detects references to known browser credential database paths (2 or more)"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $chrome  = "\\Google\\Chrome\\User Data\\Default\\Login Data" nocase
        $firefox = "\\Mozilla\\Firefox\\Profiles" nocase
        $edge    = "\\Microsoft\\Edge\\User Data\\Default\\Login Data" nocase
        $opera   = "\\Opera Software\\Opera Stable\\Login Data" nocase
    condition:
        2 of them
}

rule MPRESS_Packed_Executable {
    meta:
        description = "Detects MPRESS-packed executables by section name signature"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $mz      = { 4D 5A }
        $mpress1 = ".MPRESS1"
        $mpress2 = ".MPRESS2"
    condition:
        $mz at 0 and $mpress1 and $mpress2
}

rule Suspicious_Scheduled_Task {
    meta:
        description = "Detects executables that create persistent hidden scheduled tasks"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $sc1 = "schtasks" nocase
        $sc2 = "/create" nocase
        $sc3 = "/sc onlogon" nocase
        $sc4 = "/sc onstart" nocase
        $sc5 = "TaskScheduler" nocase
        $sc6 = "HIDDEN" nocase
    condition:
        ($sc1 and $sc2 and (1 of ($sc3,$sc4))) or ($sc5 and $sc6)
}

rule Encoded_PowerShell_Command {
    meta:
        description = "Detects base64-encoded PowerShell commands — common in fileless malware droppers"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $ps   = "powershell" nocase
        $enc1 = "-EncodedCommand" nocase
        $enc2 = " -enc " nocase
        $b64  = /[A-Za-z0-9+\/]{40,}={0,2}/
    condition:
        $ps and (1 of ($enc1,$enc2)) and $b64
}

rule Double_Extension_Executable {
    meta:
        description = "Detects PE files with double extensions in embedded strings (social engineering lure)"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $mz = { 4D 5A }
        $d1 = ".pdf.exe" nocase
        $d2 = ".doc.exe" nocase
        $d3 = ".jpg.exe" nocase
        $d4 = ".xlsx.exe" nocase
        $d5 = ".txt.exe" nocase
        $d6 = ".mp4.exe" nocase
    condition:
        $mz at 0 and any of ($d1,$d2,$d3,$d4,$d5,$d6)
}

rule Reflective_DLL_Injection {
    meta:
        description = "Detects reflective DLL injection loaders by their characteristic export name"
        author = "ReLay Shield"
        license = "Apache-2.0"
    strings:
        $a = "ReflectiveLoader"
        $b = "reflective_dll" nocase
    condition:
        any of them
}
