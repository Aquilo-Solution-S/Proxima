import { Component } from "solid-js";
import { Mono } from "./mono";

export const Timecode: Component<{
  iso: string;
}> = (props) => {
  const t = props.iso.slice(11, 19);
  return (
    <Mono dim style={{ "font-size": "10px" }}>
      {t}
    </Mono>
  );
};
