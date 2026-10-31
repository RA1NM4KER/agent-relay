import Foundation
import Virtualization

// Read-only host capability probe. Does not configure or start a VM.
let result: [String: Any] = [
    "scope": "READ_ONLY_VIRTUALIZATION_CAPABILITY",
    "supported": VZVirtualMachine.isSupported,
    "minimumMemoryBytes": VZVirtualMachineConfiguration.minimumAllowedMemorySize,
    "maximumMemoryBytes": VZVirtualMachineConfiguration.maximumAllowedMemorySize,
    "minimumCPUs": VZVirtualMachineConfiguration.minimumAllowedCPUCount,
    "maximumCPUs": VZVirtualMachineConfiguration.maximumAllowedCPUCount,
    "hostCPUs": ProcessInfo.processInfo.activeProcessorCount,
    "hostMemoryBytes": ProcessInfo.processInfo.physicalMemory,
    "vmStarted": false,
    "containmentProven": false,
]
let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys, .prettyPrinted])
FileHandle.standardOutput.write(data)
FileHandle.standardOutput.write(Data("\n".utf8))
