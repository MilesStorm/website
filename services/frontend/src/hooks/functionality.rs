use super::ROOT_DOMAIN;

pub async fn dice_prediction() -> Result<(), reqwest::Error> {
    let response = reqwest::get(format!("{}/api/login", ROOT_DOMAIN())).await;

    match response {
        Ok(_) => Ok(()),
        // Ok(res) => {
        //     let json_value: Json = res.json().await?;

        //     Ok((
        //         "tes".into(),
        //         json_value["user"]["username"]
        //             .as_str()
        //             .map(|s| s.to_string()),
        //     ))
        // }
        Err(e) => Err(e),
    }
}
