rule EICAR_Test {
    meta:
        description = "EICAR antivirus test file"
    strings:
        $s = "EICAR-STANDARD-ANTIVIRUS-TEST-FILE"
    condition:
        $s
}

rule Suspicious_Dropper {
    meta:
        description = "Generic dropper pattern"
    strings:
        $a = "This program cannot be run in DOS mode"
        $b = { 4D 5A }           // MZ header
        $c = "cmd.exe /c" nocase
    condition:
        $b at 0 and $a and $c
}
