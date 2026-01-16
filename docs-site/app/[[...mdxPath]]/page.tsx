import type { Metadata } from "next";
import { generateStaticParamsFor, importPage } from "nextra/pages";
import type { ComponentType, ReactNode } from "react";
import { useMDXComponents as getMDXComponents } from "../../mdx-components";

export const generateStaticParams = generateStaticParamsFor("mdxPath");

type PageParams = { mdxPath: string[] };
type PageProps = { params: Promise<PageParams> };

export async function generateMetadata(props: PageProps): Promise<Metadata> {
  const params = await props.params;
  const { metadata } = await importPage(params.mdxPath);
  return metadata as Metadata;
}

type WrapperProps = {
  toc: unknown;
  metadata: unknown;
  sourceCode: unknown;
  children: ReactNode;
};

const Wrapper = getMDXComponents().wrapper as ComponentType<WrapperProps>;

export default async function Page(props: PageProps) {
  const params = await props.params;
  const {
    default: MDXContent,
    toc,
    metadata,
    sourceCode,
  } = await importPage(params.mdxPath);
  return (
    <Wrapper toc={toc} metadata={metadata} sourceCode={sourceCode}>
      <MDXContent params={params} />
    </Wrapper>
  );
}
