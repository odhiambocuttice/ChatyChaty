const BRAND_NAME = "Chatelle";

const HandyTextLogo = ({
  width = 140,
  height,
  className,
}: {
  width?: number;
  height?: number;
  className?: string;
}) => {
  return (
    <svg
      width={width}
      height={height}
      className={className}
      viewBox="0 0 360 72"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      role="img"
      aria-label={BRAND_NAME}
    >
      <text
        x="50%"
        y="52"
        textAnchor="middle"
        className="logo-primary"
        style={{
          fontFamily:
            '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
          fontSize: "52px",
          fontWeight: 800,
          letterSpacing: "-0.03em",
        }}
      >
        {BRAND_NAME}
      </text>
    </svg>
  );
};

export default HandyTextLogo;
