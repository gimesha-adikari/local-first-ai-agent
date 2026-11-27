// Local line icons drawn for this app (24px grid, 1.6px stroke). No external assets.
import type { ReactNode, SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement> & { size?: number };

function Icon({ size = 16, children, ...props }: IconProps & { children: ReactNode }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor"
      strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false" {...props}>
      {children}
    </svg>
  );
}

export const PanelLeftIcon = (p: IconProps) => <Icon {...p}><rect x="3.5" y="4.5" width="17" height="15" rx="2.5" /><path d="M9 4.5v15" /></Icon>;
export const PanelRightIcon = (p: IconProps) => <Icon {...p}><rect x="3.5" y="4.5" width="17" height="15" rx="2.5" /><path d="M15 4.5v15" /></Icon>;
export const PlusIcon = (p: IconProps) => <Icon {...p}><path d="M12 5v14M5 12h14" /></Icon>;
export const ComposeIcon = (p: IconProps) => <Icon {...p}><path d="M12 4.5H7a2.5 2.5 0 0 0-2.5 2.5v10A2.5 2.5 0 0 0 7 19.5h10a2.5 2.5 0 0 0 2.5-2.5v-5" /><path d="m17.5 3.5 3 3-7.5 7.5-3.5.5.5-3.5z" /></Icon>;
export const SearchIcon = (p: IconProps) => <Icon {...p}><circle cx="11" cy="11" r="6" /><path d="m20 20-4.5-4.5" /></Icon>;
export const CubeIcon = (p: IconProps) => <Icon {...p}><path d="M12 3.5 19.5 7.5v9L12 20.5 4.5 16.5v-9z" /><path d="M4.5 7.5 12 11.5l7.5-4M12 11.5v9" /></Icon>;
export const GearIcon = (p: IconProps) => <Icon {...p}><circle cx="12" cy="12" r="2.8" /><path d="M19.4 13.5a7.6 7.6 0 0 0 0-3l1.6-1.3-1.8-3.1-2 .7a7.5 7.5 0 0 0-2.6-1.5L14.2 3h-3.6l-.4 2.3a7.5 7.5 0 0 0-2.6 1.5l-2-.7-1.8 3.1 1.6 1.3a7.6 7.6 0 0 0 0 3l-1.6 1.3 1.8 3.1 2-.7a7.5 7.5 0 0 0 2.6 1.5l.4 2.3h3.6l.4-2.3a7.5 7.5 0 0 0 2.6-1.5l2 .7 1.8-3.1z" /></Icon>;
export const ChatIcon = (p: IconProps) => <Icon {...p}><path d="M5 5.5h14a1.5 1.5 0 0 1 1.5 1.5v8.5A1.5 1.5 0 0 1 19 17h-8l-4.5 3.5V17H5a1.5 1.5 0 0 1-1.5-1.5V7A1.5 1.5 0 0 1 5 5.5z" /></Icon>;
export const AgentIcon = (p: IconProps) => <Icon {...p}><path d="M12 3.5v3M12 17.5v3M3.5 12h3M17.5 12h3" /><circle cx="12" cy="12" r="4.5" /><circle cx="12" cy="12" r="1.2" fill="currentColor" stroke="none" /></Icon>;
export const FileIcon = (p: IconProps) => <Icon {...p}><path d="M13.5 3.5H7A1.5 1.5 0 0 0 5.5 5v14A1.5 1.5 0 0 0 7 20.5h10a1.5 1.5 0 0 0 1.5-1.5V8.5z" /><path d="M13.5 3.5v5h5" /></Icon>;
export const FolderIcon = (p: IconProps) => <Icon {...p}><path d="M3.5 7A1.5 1.5 0 0 1 5 5.5h4.2l2 2.2H19A1.5 1.5 0 0 1 20.5 9.2V17a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 17z" /></Icon>;
export const CloseIcon = (p: IconProps) => <Icon {...p}><path d="m6.5 6.5 11 11M17.5 6.5l-11 11" /></Icon>;
export const CheckIcon = (p: IconProps) => <Icon {...p}><path d="m5 12.5 4.5 4.5L19 7.5" /></Icon>;
export const ChevronDownIcon = (p: IconProps) => <Icon {...p}><path d="m7 10 5 5 5-5" /></Icon>;
export const ChevronRightIcon = (p: IconProps) => <Icon {...p}><path d="m10 7 5 5-5 5" /></Icon>;
export const ArrowUpIcon = (p: IconProps) => <Icon {...p}><path d="M12 19V5M6 11l6-6 6 6" /></Icon>;
export const ArrowDownIcon = (p: IconProps) => <Icon {...p}><path d="M12 5v14M6 13l6 6 6-6" /></Icon>;
export const StopIcon = (p: IconProps) => <Icon {...p}><rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor" stroke="none" /></Icon>;
export const CopyIcon = (p: IconProps) => <Icon {...p}><rect x="8.5" y="8.5" width="11" height="11" rx="2" /><path d="M15.5 8.5V6A1.5 1.5 0 0 0 14 4.5H6A1.5 1.5 0 0 0 4.5 6v8A1.5 1.5 0 0 0 6 15.5h2.5" /></Icon>;
export const RefreshIcon = (p: IconProps) => <Icon {...p}><path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" /><path d="M19.5 4.5v4h-4" /></Icon>;
export const MoreIcon = (p: IconProps) => <Icon {...p}><circle cx="6" cy="12" r="1" fill="currentColor" /><circle cx="12" cy="12" r="1" fill="currentColor" /><circle cx="18" cy="12" r="1" fill="currentColor" /></Icon>;
export const SlidersIcon = (p: IconProps) => <Icon {...p}><path d="M4.5 7h9M17.5 7h2M4.5 17h3M11.5 17h8" /><circle cx="15.5" cy="7" r="2" /><circle cx="9.5" cy="17" r="2" /></Icon>;
export const ThinkIcon = (p: IconProps) => <Icon {...p}><path d="M9 18.5h6M10 21h4" /><path d="M12 3.5a6 6 0 0 0-3.5 10.9c.6.4 1 1.1 1 1.9v.2h5v-.2c0-.8.4-1.5 1-1.9A6 6 0 0 0 12 3.5z" /></Icon>;
export const AlertIcon = (p: IconProps) => <Icon {...p}><path d="M12 4 21 19.5H3z" /><path d="M12 10v4.5M12 17.2v.1" /></Icon>;
export const InfoIcon = (p: IconProps) => <Icon {...p}><circle cx="12" cy="12" r="8.5" /><path d="M12 11v5.5M12 7.8v.1" /></Icon>;
export const ShieldIcon = (p: IconProps) => <Icon {...p}><path d="M12 3.5 19 6v5.5c0 4.3-3 7.6-7 9-4-1.4-7-4.7-7-9V6z" /><path d="m9 12 2.2 2.2L15.5 10" /></Icon>;
export const LockIcon = (p: IconProps) => <Icon {...p}><rect x="5" y="10.5" width="14" height="10" rx="2" /><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5" /></Icon>;
export const TrashIcon = (p: IconProps) => <Icon {...p}><path d="M4.5 7h15M9.5 7V5h5v2M6.5 7l1 12.5h9l1-12.5" /></Icon>;
export const EditIcon = (p: IconProps) => <Icon {...p}><path d="m14.5 5.5 4 4L9 19H5v-4z" /></Icon>;
export const NoteIcon = (p: IconProps) => <Icon {...p}><path d="M6.5 3.5h11A1.5 1.5 0 0 1 19 5v14a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1.5-1.5z" /><path d="M8.5 8h7M8.5 12h7M8.5 16h4" /></Icon>;
export const ClockIcon = (p: IconProps) => <Icon {...p}><circle cx="12" cy="12" r="8.5" /><path d="M12 7.5V12l3 2" /></Icon>;
export const GridIcon = (p: IconProps) => <Icon {...p}><rect x="4" y="4" width="6.5" height="6.5" rx="1.5" /><rect x="13.5" y="4" width="6.5" height="6.5" rx="1.5" /><rect x="4" y="13.5" width="6.5" height="6.5" rx="1.5" /><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.5" /></Icon>;
export const CpuIcon = (p: IconProps) => <Icon {...p}><rect x="6.5" y="6.5" width="11" height="11" rx="1.5" /><rect x="9.5" y="9.5" width="5" height="5" rx=".5" /><path d="M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3" /></Icon>;
export const CalculatorIcon = (p: IconProps) => <Icon {...p}><rect x="5.5" y="3.5" width="13" height="17" rx="2" /><path d="M8.5 7.5h7M8.5 12h.1M12 12h.1M15.5 12h.1M8.5 16h.1M12 16h.1M15.5 16h.1" /></Icon>;
export const LaunchIcon = (p: IconProps) => <Icon {...p}><path d="M13.5 4.5h6v6M19.5 4.5l-8 8" /><path d="M17.5 13.5V18a1.5 1.5 0 0 1-1.5 1.5H6A1.5 1.5 0 0 1 4.5 18V8A1.5 1.5 0 0 1 6 6.5h4.5" /></Icon>;
export const SunIcon = (p: IconProps) => <Icon {...p}><circle cx="12" cy="12" r="3.5" /><path d="M12 3.5v2M12 18.5v2M3.5 12h2M18.5 12h2M6 6l1.4 1.4M16.6 16.6 18 18M6 18l1.4-1.4M16.6 7.4 18 6" /></Icon>;
export const MoonIcon = (p: IconProps) => <Icon {...p}><path d="M19.5 14.5A7.5 7.5 0 0 1 9.5 4.5a7.5 7.5 0 1 0 10 10z" /></Icon>;
export const MonitorIcon = (p: IconProps) => <Icon {...p}><rect x="3.5" y="4.5" width="17" height="11.5" rx="1.5" /><path d="M9 20h6M12 16v4" /></Icon>;
export const UploadIcon = (p: IconProps) => <Icon {...p}><path d="M12 15.5v-11M7.5 9 12 4.5 16.5 9" /><path d="M4.5 15v3.5A1.5 1.5 0 0 0 6 20h12a1.5 1.5 0 0 0 1.5-1.5V15" /></Icon>;
export const CloudIcon = (p: IconProps) => <Icon {...p}><path d="M7.5 18.5h10a4 4 0 0 0 .6-7.95A5.5 5.5 0 0 0 7.4 9.1 4.75 4.75 0 0 0 7.5 18.5z" /></Icon>;
export const PowerIcon = (p: IconProps) => <Icon {...p}><path d="M12 3.5v8" /><path d="M7 6.5a7 7 0 1 0 10 0" /></Icon>;

export const BookIcon = (p: IconProps) => <Icon {...p}><path d="M5 5.5A1.5 1.5 0 0 1 6.5 4H18.5v13H6.5A1.5 1.5 0 0 0 5 18.5z" /><path d="M5 18.5A1.5 1.5 0 0 0 6.5 20h12v-3" /><path d="M9 8h6" /></Icon>;
export const MemoryIcon = (p: IconProps) => <Icon {...p}><path d="M7 3.5h10A1.5 1.5 0 0 1 18.5 5v15.5L12 16.5l-6.5 4V5A1.5 1.5 0 0 1 7 3.5z" /></Icon>;
export const QuoteIcon = (p: IconProps) => <Icon {...p}><path d="M5 17.5c2.5-.5 4-2.5 4-5.5V7.5H5V12h4M15 17.5c2.5-.5 4-2.5 4-5.5V7.5h-4V12h4" /></Icon>;

export function Spinner({ size = 14 }: { size?: number }) {
  return (
    <svg className="spinner" width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeOpacity=".25" strokeWidth="2.4" />
      <path d="M21 12a9 9 0 0 0-9-9" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" />
    </svg>
  );
}
export const CropIcon = (p: IconProps) => <Icon {...p}><path d="M4.5 9V5.5a1 1 0 0 1 1-1H9M15 4.5h3.5a1 1 0 0 1 1 1V9M19.5 15v3.5a1 1 0 0 1-1 1H15M9 19.5H5.5a1 1 0 0 1-1-1V15" /><rect x="8.5" y="8.5" width="7" height="7" rx="1" /></Icon>;
export const ImageIcon = (p: IconProps) => <Icon {...p}><rect x="3.5" y="5" width="17" height="14" rx="2" /><circle cx="9" cy="10" r="1.6" /><path d="m4.5 17.5 4.5-4.5 3.5 3.5 2.5-2.5 4.5 4" /></Icon>;
export const ScanTextIcon = (p: IconProps) => <Icon {...p}><path d="M4.5 8.5V6a1.5 1.5 0 0 1 1.5-1.5h2.5M15.5 4.5H18A1.5 1.5 0 0 1 19.5 6v2.5M19.5 15.5V18a1.5 1.5 0 0 1-1.5 1.5h-2.5M8.5 19.5H6A1.5 1.5 0 0 1 4.5 18v-2.5" /><path d="M8.5 9.5h7M8.5 12h7M8.5 14.5h4" /></Icon>;
export const GlobeIcon = (p: IconProps) => <Icon {...p}><circle cx="12" cy="12" r="8.5" /><path d="M3.5 12h17M12 3.5c2.4 2.3 3.6 5.1 3.6 8.5s-1.2 6.2-3.6 8.5c-2.4-2.3-3.6-5.1-3.6-8.5S9.6 5.8 12 3.5z" /></Icon>;
export const ExternalIcon = (p: IconProps) => <Icon {...p}><path d="M13.5 4.5h6v6M19.5 4.5l-8 8" /><path d="M18 14v4a1.5 1.5 0 0 1-1.5 1.5H6A1.5 1.5 0 0 1 4.5 18V7.5A1.5 1.5 0 0 1 6 6h4" /></Icon>;
export const PaperclipIcon = (p: IconProps) => <Icon {...p}><path d="m19 11.5-7.1 7.1a4.5 4.5 0 0 1-6.4-6.4l7.4-7.4a3 3 0 0 1 4.2 4.2l-7.3 7.3a1.5 1.5 0 0 1-2.1-2.1l6.6-6.6" /></Icon>;
